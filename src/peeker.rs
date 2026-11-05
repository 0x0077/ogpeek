//! Fetching pages: the client, the cache, and request coalescing.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE, HeaderName, HeaderValue, LOCATION};
use tokio::sync::OnceCell;
use url::Url;

use crate::net::{Error, Resolver, forbidden_ip};
use crate::oembed::{oembed_endpoint, parse_oembed};
use crate::page::{Embed, Media, Page};
use crate::parse::{decode_html, head_end, parse_bytes, parse_document};

const ACCEPT_HTML: &str = "text/html,application/xhtml+xml;q=0.9,*/*;q=0.5";

/// Raw bytes from [`Peeker::fetch`] or [`Peeker::fetch_head`].
#[derive(Debug, Clone)]
pub struct Fetched {
    /// The address after redirects.
    pub url: Url,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    /// True when reading stopped before the end of the response.
    pub partial: bool,
}

/// Fetches link previews.
///
/// Cheap to clone; clones share the connection pool, DNS cache and preview cache. Make one
/// and keep it for the life of the program.
///
/// ```no_run
/// # async fn run() -> Result<(), ogpeek::Error> {
/// let peeker = ogpeek::Peeker::new();
/// let page = peeker.peek("https://www.rust-lang.org").await?;
/// println!("{:?} {:?}", page.title, page.image().map(|i| &i.url));
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct Peeker {
    inner: Arc<Inner>,
}

struct Inner {
    client: reqwest::Client,
    opts: Builder,
    cache: Mutex<Cache>,
    flights: Mutex<HashMap<String, Flight>>,
    counters: Counters,
}

#[derive(Default)]
struct Counters {
    hits: AtomicU64,
    stale_hits: AtomicU64,
    fetches: AtomicU64,
    errors: AtomicU64,
    stale_on_error: AtomicU64,
    retries: AtomicU64,
}

/// What a [`Peeker`] has done since it was built. See [`Peeker::stats`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Previews in the cache now, fresh or stale.
    pub cached: usize,
    /// Fetches running now.
    pub in_flight: usize,
    /// Answered from a fresh cache entry.
    pub hits: u64,
    /// Answered from a stale entry while a refresh ran in the background.
    pub stale_hits: u64,
    /// Network loads started (concurrent requests for one URL count once).
    pub fetches: u64,
    /// Loads that failed.
    pub errors: u64,
    /// Failed loads answered with the last good preview instead.
    pub stale_on_error: u64,
    /// Requests retried after a connection failure or a 502, 503 or 504.
    pub retries: u64,
}

type Flight = Arc<OnceCell<Result<Arc<Page>, Error>>>;

/// Settings for a [`Peeker`].
#[derive(Clone, Debug)]
pub struct Builder {
    user_agent: String,
    timeout: Duration,
    connect_timeout: Duration,
    max_head_bytes: usize,
    max_body_bytes: usize,
    max_redirects: usize,
    allow_private: bool,
    any_port: bool,
    cache_ttl: Duration,
    error_ttl: Duration,
    stale_ttl: Duration,
    retry: bool,
    cache_capacity: usize,
    accept_language: Option<String>,
    follow_refresh: bool,
    oembed: bool,
    host_headers: Vec<(String, HeaderName, HeaderValue)>,
}

impl Default for Builder {
    fn default() -> Self {
        Builder {
            user_agent: concat!(
                "Mozilla/5.0 (compatible; ogpeek/",
                env!("CARGO_PKG_VERSION"),
                "; +https://github.com/0x0077/ogpeek)"
            )
            .to_owned(),
            timeout: Duration::from_secs(8),
            connect_timeout: Duration::from_secs(3),
            max_head_bytes: 1 << 20,
            max_body_bytes: 2 << 20,
            max_redirects: 5,
            allow_private: false,
            any_port: false,
            cache_ttl: Duration::from_secs(600),
            error_ttl: Duration::from_secs(30),
            stale_ttl: Duration::from_secs(3600),
            retry: true,
            cache_capacity: 4096,
            accept_language: None,
            follow_refresh: true,
            oembed: true,
            host_headers: Vec::new(),
        }
    }
}

impl Builder {
    /// The `User-Agent` sent with every request.
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = ua.into();
        self
    }
    /// Limit for one whole peek: every redirect, retry, the page and the oEmbed answer
    /// together. Default 8 seconds. A peek never takes longer than this.
    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }
    /// Limit for opening a connection. Default 3 seconds.
    pub fn connect_timeout(mut self, t: Duration) -> Self {
        self.connect_timeout = t;
        self
    }
    /// Most bytes read while looking for the end of `<head>`. Default 1 MiB (some video
    /// sites put their tags after 750 KiB of head). Reading stops earlier as soon as the
    /// title, description, image and an icon have all been seen.
    pub fn max_head_bytes(mut self, n: usize) -> Self {
        self.max_head_bytes = n;
        self
    }
    /// Most bytes read when the head says nothing and the body must be searched, and the
    /// default for [`Peeker::fetch`]. Default 2 MiB.
    pub fn max_body_bytes(mut self, n: usize) -> Self {
        self.max_body_bytes = n;
        self
    }
    /// Default 5.
    pub fn max_redirects(mut self, n: usize) -> Self {
        self.max_redirects = n;
        self
    }
    /// Allow internal addresses and any port. Only for tests and trusted input.
    pub fn allow_private(mut self, yes: bool) -> Self {
        self.allow_private = yes;
        self.any_port = yes;
        self
    }
    /// Allow URLs with explicit ports such as `:8080`. Off by default because public pages
    /// almost never need one and internal services often listen on one.
    pub fn any_port(mut self, yes: bool) -> Self {
        self.any_port = yes;
        self
    }
    /// How long a preview stays cached. Default 10 minutes; zero turns caching off.
    pub fn cache_ttl(mut self, t: Duration) -> Self {
        self.cache_ttl = t;
        self
    }
    /// How long a failure stays cached, so a dead link is not fetched again on every
    /// request. Default 30 seconds.
    /// How long after expiring a preview may still be served. Within this window a peek
    /// answers at once with the old preview and refreshes it in the background, and a
    /// failed refresh keeps the old preview instead of returning the error. Default 1 hour;
    /// zero turns it off.
    pub fn stale_ttl(mut self, t: Duration) -> Self {
        self.stale_ttl = t;
        self
    }
    /// Retry once, after 200 ms, when a connection fails or the server answers 502, 503
    /// or 504. Default on. The retry still has to fit in the timeout.
    pub fn retry(mut self, yes: bool) -> Self {
        self.retry = yes;
        self
    }
    pub fn error_ttl(mut self, t: Duration) -> Self {
        self.error_ttl = t;
        self
    }
    /// Most previews kept in memory. Default 4096; the oldest go first.
    pub fn cache_capacity(mut self, n: usize) -> Self {
        self.cache_capacity = n;
        self
    }
    /// Sends `Accept-Language`, so sites answer in that language when they can.
    pub fn accept_language(mut self, v: impl Into<String>) -> Self {
        self.accept_language = Some(v.into());
        self
    }
    /// Follow `<meta http-equiv=refresh>` once when the page has nothing else. Default on.
    /// Use oEmbed. Default on: for the built-in providers (YouTube, TikTok, Vimeo, Spotify,
    /// SoundCloud, X posts and others, see [`crate::oembed_hosts`]) the endpoint is asked
    /// alongside the page and its answer wins for title, image and author; for any other
    /// page, an advertised endpoint is asked only when the page lacks a title or image.
    pub fn oembed(mut self, yes: bool) -> Self {
        self.oembed = yes;
        self
    }
    pub fn follow_refresh(mut self, yes: bool) -> Self {
        self.follow_refresh = yes;
        self
    }
    /// Adds a header for one exact host only, such as an API token for `api.github.com`.
    /// It is not sent after a redirect to another host.
    ///
    /// # Panics
    /// If `name` or `value` is not a valid header.
    pub fn host_header(mut self, host: impl Into<String>, name: &str, value: &str) -> Self {
        let name = HeaderName::try_from(name).expect("header name");
        let mut value = HeaderValue::try_from(value).expect("header value");
        value.set_sensitive(true);
        self.host_headers.push((host.into(), name, value));
        self
    }

    pub fn build(self) -> Result<Peeker, Error> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(self.timeout)
            .connect_timeout(self.connect_timeout)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_nodelay(true)
            .dns_resolver(Arc::new(if self.allow_private {
                Resolver::prefer_ipv4()
            } else {
                Resolver::safe()
            }))
            .user_agent(self.user_agent.clone())
            .build()
            .map_err(Error::from)?;
        Ok(Peeker {
            inner: Arc::new(Inner {
                client,
                cache: Mutex::new(Cache::new(self.cache_capacity)),
                opts: self,
                flights: Mutex::new(HashMap::new()),
                counters: Counters::default(),
            }),
        })
    }
}

impl Default for Peeker {
    fn default() -> Self {
        Self::new()
    }
}

impl Peeker {
    /// A peeker with default settings.
    ///
    /// # Panics
    /// If the TLS backend cannot start, which does not happen on supported platforms.
    pub fn new() -> Self {
        Builder::default().build().expect("ogpeek client")
    }

    pub fn builder() -> Builder {
        Builder::default()
    }

    /// The underlying HTTP client, with the safe resolver and connection pool.
    ///
    /// Requests made with it directly skip the URL checks in [`Peeker::fetch`] (scheme,
    /// literal IP addresses, ports, redirects); DNS-based blocking still applies.
    pub fn client(&self) -> &reqwest::Client {
        &self.inner.client
    }

    /// The preview for `url`: from the cache when fresh, otherwise fetched.
    ///
    /// Concurrent calls for the same URL share one request. A preview past its cache time
    /// but within [`Builder::stale_ttl`] is returned at once while a fresh copy is fetched
    /// in the background.
    pub async fn peek(&self, url: &str) -> Result<Arc<Page>, Error> {
        let key = cache_key(url)?;
        let c = &self.inner.counters;
        let found = self.inner.cache.lock().expect("cache").lookup(&key);
        match found {
            Lookup::Fresh(r) => {
                c.hits.fetch_add(1, Relaxed);
                r
            }
            Lookup::Stale(p) => {
                c.stale_hits.fetch_add(1, Relaxed);
                if !self
                    .inner
                    .flights
                    .lock()
                    .expect("flights")
                    .contains_key(&key)
                {
                    let me = self.clone();
                    tokio::spawn(async move {
                        let _ = me.coalesced(key).await;
                    });
                }
                Ok(p)
            }
            Lookup::Miss => self.coalesced(key).await,
        }
    }

    /// Fetches `url` even when a cached copy exists, and caches the result.
    pub async fn peek_fresh(&self, url: &str) -> Result<Arc<Page>, Error> {
        let key = cache_key(url)?;
        self.coalesced(key).await
    }

    /// The cached preview for `url`, fresh or stale. Never fetches.
    pub fn cached(&self, url: &str) -> Option<Arc<Page>> {
        let key = cache_key(url).ok()?;
        self.inner.cache.lock().expect("cache").last_good(&key)
    }

    /// Counts of cache hits, fetches, errors and retries, for logs and dashboards.
    pub fn stats(&self) -> Stats {
        let c = &self.inner.counters;
        Stats {
            cached: self.inner.cache.lock().expect("cache").map.len(),
            in_flight: self.inner.flights.lock().expect("flights").len(),
            hits: c.hits.load(Relaxed),
            stale_hits: c.stale_hits.load(Relaxed),
            fetches: c.fetches.load(Relaxed),
            errors: c.errors.load(Relaxed),
            stale_on_error: c.stale_on_error.load(Relaxed),
            retries: c.retries.load(Relaxed),
        }
    }

    async fn coalesced(&self, key: String) -> Result<Arc<Page>, Error> {
        let cell = self
            .inner
            .flights
            .lock()
            .expect("flights")
            .entry(key.clone())
            .or_default()
            .clone();
        // If every caller gives up before the load finishes, the guard removes the entry
        // so abandoned URLs do not pile up.
        let guard = FlightGuard {
            inner: &self.inner,
            key: &key,
            cell: &cell,
        };
        let result = cell
            .get_or_init(|| async {
                self.inner.counters.fetches.fetch_add(1, Relaxed);
                self.within(self.load(&key)).await.map(Arc::new)
            })
            .await
            .clone();
        drop(guard);
        let mut flights = self.inner.flights.lock().expect("flights");
        if !flights.get(&key).is_some_and(|c| Arc::ptr_eq(c, &cell)) {
            return result;
        }
        flights.remove(&key);
        let opts = &self.inner.opts;
        let mut cache = self.inner.cache.lock().expect("cache");
        match result {
            Ok(p) => {
                cache.put(key, Ok(p.clone()), opts.cache_ttl, opts.stale_ttl);
                Ok(p)
            }
            Err(e) => {
                self.inner.counters.errors.fetch_add(1, Relaxed);
                if let Some(old) = cache.last_good(&key) {
                    self.inner.counters.stale_on_error.fetch_add(1, Relaxed);
                    // Serve the old preview as fresh for the error time, so a dead site is
                    // not asked again on every request; it still expires on its own date.
                    cache.keep(&key, opts.error_ttl);
                    return Ok(old);
                }
                cache.put(key, Err(e.clone()), opts.error_ttl, Duration::ZERO);
                Err(e)
            }
        }
    }

    async fn load(&self, start: &str) -> Result<Page, Error> {
        let opts = &self.inner.opts;
        if opts.oembed
            && let Some(endpoint) = oembed_endpoint(start)
        {
            // Known provider: ask both at once. The provider often answers when its page
            // is a login wall or a script shell.
            let (page, embed) = tokio::join!(self.load_page(start), self.ask_oembed(&endpoint));
            return match (page, embed) {
                (Ok(mut p), Some(e)) => {
                    p.merge_embed(e, true);
                    Ok(p)
                }
                (Ok(p), None) => Ok(p),
                (Err(_), Some(e)) => {
                    let mut p = Page {
                        url: start.to_owned(),
                        ..Default::default()
                    };
                    p.merge_embed(e, true);
                    Ok(p)
                }
                (Err(e), None) => Err(e),
            };
        }
        let mut page = self.load_page(start).await?;
        if opts.oembed
            && (page.title.is_none() || page.images.is_empty())
            && let Some(endpoint) = page.oembed.clone()
            && let Some(e) = self.ask_oembed(&endpoint).await
        {
            page.merge_embed(e, false);
        }
        Ok(page)
    }

    async fn ask_oembed(&self, endpoint: &str) -> Option<Embed> {
        let f = self
            .fetch_inner(endpoint, "application/json", 256 << 10)
            .await
            .ok()?;
        parse_oembed(&f.body)
    }

    async fn load_page(&self, start: &str) -> Result<Page, Error> {
        let opts = &self.inner.opts;
        let mut target = start.to_owned();
        for hop in 0..2 {
            let (url, mut res) = self.open(&target, ACCEPT_HTML).await?;
            let ct = content_type(&res);
            if !is_html(ct.as_deref()) {
                return Ok(other(url, ct));
            }
            let (mut body, stopped) = self.read_head(&mut res, &url, ct.as_deref()).await?;
            let mut page = parse_bytes(&body, url.as_str(), ct.as_deref());
            if page.is_empty() && stopped {
                while body.len() < opts.max_body_bytes {
                    match res.chunk().await {
                        Ok(Some(c)) => body.extend_from_slice(&c),
                        _ => break,
                    }
                }
                page = parse_document(&decode_html(&body, ct.as_deref()), url.as_str());
                page.content_type = ct;
            }
            if is_challenge(&page) {
                return Err(Error::Challenge);
            }
            if hop == 0
                && opts.follow_refresh
                && page.is_empty()
                && let Some(next) = page.refresh.clone()
            {
                target = next;
                continue;
            }
            return Ok(page);
        }
        Err(Error::TooManyRedirects)
    }

    /// Fetches `url` through the same protections and returns up to `max` bytes of the
    /// body. For feeds, JSON APIs and images that go with a preview.
    pub async fn fetch(&self, url: &str, accept: &str, max: usize) -> Result<Fetched, Error> {
        self.within(self.fetch_inner(url, accept, max)).await
    }

    async fn fetch_inner(&self, url: &str, accept: &str, max: usize) -> Result<Fetched, Error> {
        let (url, mut res) = self.open(url, accept).await?;
        let content_type = content_type(&res);
        let mut body = Vec::new();
        let mut partial = false;
        while let Some(chunk) = res.chunk().await? {
            body.extend_from_slice(&chunk);
            if body.len() > max {
                body.truncate(max);
                partial = true;
                break;
            }
        }
        Ok(Fetched {
            url,
            content_type,
            body,
            partial,
        })
    }

    /// Fetches an HTML page up to the end of its `<head>`, or less: reading stops as soon
    /// as the title, description, image and icon have been seen (at most `max_head_bytes`).
    pub async fn fetch_head(&self, url: &str) -> Result<Fetched, Error> {
        self.within(self.fetch_head_inner(url)).await
    }

    /// Runs `f` within the timeout, redirects and retries included.
    async fn within<T>(&self, f: impl Future<Output = Result<T, Error>>) -> Result<T, Error> {
        tokio::time::timeout(self.inner.opts.timeout, f)
            .await
            .unwrap_or(Err(Error::Timeout))
    }

    async fn fetch_head_inner(&self, url: &str) -> Result<Fetched, Error> {
        let (url, mut res) = self.open(url, ACCEPT_HTML).await?;
        let content_type = content_type(&res);
        let (body, partial) = self
            .read_head(&mut res, &url, content_type.as_deref())
            .await?;
        Ok(Fetched {
            url,
            content_type,
            body,
            partial,
        })
    }

    /// Reads until the head ends, the preview is complete, or the limit. Returns the bytes
    /// and whether reading stopped early.
    async fn read_head(
        &self,
        res: &mut reqwest::Response,
        url: &Url,
        ct: Option<&str>,
    ) -> Result<(Vec<u8>, bool), Error> {
        let max = self.inner.opts.max_head_bytes;
        let mut body = Vec::with_capacity(16 << 10);
        // Some heads run to hundreds of kilobytes of scripts after the tags we need. Check
        // whether the preview is already complete at 16 KiB, then each time the buffer has
        // grown by a quarter, so a 1 MiB head is parsed about twenty times, not sixty.
        let mut next_check = 16 << 10;
        while let Some(chunk) = res.chunk().await? {
            let from = body.len().saturating_sub(6);
            body.extend_from_slice(&chunk);
            if head_end(&body[from..]).is_some() || body.len() >= max {
                return Ok((body, true));
            }
            if body.len() >= next_check {
                next_check = (body.len() + (16 << 10)).max(body.len() * 5 / 4);
                let upto = memchr::memrchr(b'>', &body).map_or(0, |i| i + 1);
                if complete(&parse_bytes(&body[..upto], url.as_str(), ct), body.len()) {
                    return Ok((body, true));
                }
            }
        }
        Ok((body, false))
    }

    async fn retry_after_pause(&self) {
        self.inner.counters.retries.fetch_add(1, Relaxed);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    fn check(&self, url: &Url) -> Result<(), Error> {
        let opts = &self.inner.opts;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(Error::InvalidUrl);
        }
        if url.port().is_some() && !opts.any_port {
            return Err(Error::Blocked);
        }
        let ip = match url.host() {
            None => return Err(Error::InvalidUrl),
            Some(url::Host::Ipv4(ip)) => Some(IpAddr::V4(ip)),
            Some(url::Host::Ipv6(ip)) => Some(IpAddr::V6(ip)),
            Some(url::Host::Domain(_)) => None,
        };
        if !opts.allow_private && ip.is_some_and(forbidden_ip) {
            return Err(Error::Blocked);
        }
        Ok(())
    }

    /// Sends a GET and follows redirects, checking every hop.
    async fn open(&self, start: &str, accept: &str) -> Result<(Url, reqwest::Response), Error> {
        let opts = &self.inner.opts;
        let mut url = Url::parse(start).map_err(|_| Error::InvalidUrl)?;
        for _ in 0..=opts.max_redirects {
            self.check(&url)?;
            let mut req = self.inner.client.get(url.clone()).header(ACCEPT, accept);
            if let Some(l) = &opts.accept_language {
                req = req.header(ACCEPT_LANGUAGE, l);
            }
            for (host, name, value) in &opts.host_headers {
                if url.host_str() == Some(host.as_str()) {
                    req = req.header(name, value);
                }
            }
            let again = req.try_clone().filter(|_| opts.retry);
            let res = match (req.send().await, again) {
                (Ok(r), Some(again)) if matches!(r.status().as_u16(), 502..=504) => {
                    self.retry_after_pause().await;
                    again.send().await?
                }
                (Err(e), Some(again)) if e.is_connect() => {
                    self.retry_after_pause().await;
                    again.send().await?
                }
                (first, _) => first?,
            };
            let status = res.status();
            if status.is_redirection()
                && let Some(loc) = res.headers().get(LOCATION).and_then(|v| v.to_str().ok())
            {
                url = url.join(loc).map_err(|_| Error::InvalidUrl)?;
                continue;
            }
            if !status.is_success() {
                return Err(Error::Status(status.as_u16()));
            }
            return Ok((url, res));
        }
        Err(Error::TooManyRedirects)
    }
}

/// Everything a preview card shows is there: title, description, image and a declared
/// icon. Past 64 KiB an icon is not worth waiting for; `/favicon.ico` is the fallback.
fn complete(p: &Page, read: usize) -> bool {
    p.title.is_some()
        && p.description.is_some()
        && !p.images.is_empty()
        && (!p.icons.is_empty() || read >= 64 << 10)
}

/// A bot check or access-denied page: a title from the list below and no Open Graph or
/// Twitter image. Real pages that happen to use these words carry their own tags.
fn is_challenge(p: &Page) -> bool {
    const TITLES: &[&str] = &[
        "just a moment",
        "attention required",
        "access denied",
        "access to this page has been denied",
        "robot check",
        "are you a robot",
        "are you a human",
        "human verification",
        "security check",
        "pardon our interruption",
        "please wait",
        "ddos-guard",
        "verify you are human",
        "验证码",
        "安全验证",
        "人机验证",
        "访问验证",
        "sina visitor system",
    ];
    let Some(t) = &p.title else { return false };
    let t = t.to_lowercase();
    // The phrase opens the title and is followed by nothing or by punctuation, as in
    // "Just a moment...", "Attention Required! | Cloudflare", "验证码_哔哩哔哩".
    let opens = |x: &str| {
        t.strip_prefix(x).is_some_and(|rest| {
            rest.trim_start()
                .chars()
                .next()
                .is_none_or(|c| c.is_ascii_punctuation() || "…｜：".contains(c))
        })
    };
    p.images.is_empty() && p.description.is_none() && TITLES.iter().any(|x| opens(x))
}

/// Removes an abandoned flight: one whose every caller was dropped before it finished.
struct FlightGuard<'a> {
    inner: &'a Inner,
    key: &'a str,
    cell: &'a Flight,
}

impl Drop for FlightGuard<'_> {
    fn drop(&mut self) {
        if self.cell.initialized() {
            return;
        }
        let mut flights = self.inner.flights.lock().expect("flights");
        // Only this caller and the map hold the cell: nobody else is waiting on it.
        if flights
            .get(self.key)
            .is_some_and(|c| Arc::ptr_eq(c, self.cell) && Arc::strong_count(c) <= 2)
        {
            flights.remove(self.key);
        }
    }
}

fn content_type(res: &reqwest::Response) -> Option<String> {
    res.headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

fn is_html(ct: Option<&str>) -> bool {
    let Some(ct) = ct else { return true };
    let mime = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    mime.is_empty()
        || mime == "text/html"
        || mime == "application/xhtml+xml"
        || mime == "text/plain"
}

/// A preview for something that is not a web page: an image shows itself.
fn other(url: Url, ct: Option<String>) -> Page {
    let mime = ct
        .as_deref()
        .and_then(|c| c.split(';').next())
        .map(|m| m.trim().to_ascii_lowercase());
    let image = mime.as_deref().is_some_and(|m| m.starts_with("image/"));
    Page {
        title: url
            .path_segments()
            .and_then(|mut s| s.next_back())
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        kind: mime.as_deref().map(|m| {
            if image {
                "image"
            } else {
                m.split('/').next().unwrap_or(m)
            }
            .to_owned()
        }),
        images: if image {
            vec![Media {
                url: url.to_string(),
                mime: mime.clone(),
                ..Default::default()
            }]
        } else {
            vec![]
        },
        url: url.into(),
        content_type: ct,
        ..Default::default()
    }
}

fn cache_key(url: &str) -> Result<String, Error> {
    let mut u = Url::parse(url.trim()).map_err(|_| Error::InvalidUrl)?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(Error::InvalidUrl);
    }
    u.set_fragment(None);
    Ok(u.into())
}

struct Cache {
    capacity: usize,
    map: HashMap<String, Entry>,
    order: VecDeque<String>,
}

struct Entry {
    at: Instant,
    ttl: Duration,
    /// After this, not even a stale copy is served.
    until: Instant,
    value: Result<Arc<Page>, Error>,
}

enum Lookup {
    Fresh(Result<Arc<Page>, Error>),
    Stale(Arc<Page>),
    Miss,
}

impl Cache {
    fn new(capacity: usize) -> Self {
        Cache {
            capacity,
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn lookup(&mut self, key: &str) -> Lookup {
        let Some(e) = self.map.get(key) else {
            return Lookup::Miss;
        };
        let now = Instant::now();
        match &e.value {
            r if now < e.at + e.ttl && now < e.until => Lookup::Fresh(r.clone()),
            Ok(p) if now < e.until => Lookup::Stale(p.clone()),
            _ => {
                self.map.remove(key);
                Lookup::Miss
            }
        }
    }

    /// The last successful preview, fresh or within the stale window.
    fn last_good(&self, key: &str) -> Option<Arc<Page>> {
        let e = self.map.get(key)?;
        match &e.value {
            Ok(p) if Instant::now() < e.until => Some(p.clone()),
            _ => None,
        }
    }

    /// Marks a kept entry fresh again for `ttl`, without moving its final expiry.
    fn keep(&mut self, key: &str, ttl: Duration) {
        if let Some(e) = self.map.get_mut(key) {
            e.at = Instant::now();
            e.ttl = ttl;
        }
    }

    fn put(
        &mut self,
        key: String,
        value: Result<Arc<Page>, Error>,
        ttl: Duration,
        stale: Duration,
    ) {
        if self.capacity == 0 || ttl.is_zero() {
            return;
        }
        let at = Instant::now();
        let until = at + ttl + if value.is_ok() { stale } else { Duration::ZERO };
        let entry = Entry {
            at,
            ttl,
            until,
            value,
        };
        if self.map.insert(key.clone(), entry).is_none() {
            self.order.push_back(key);
        }
        while self.map.len() > self.capacity {
            match self.order.pop_front() {
                Some(old) => {
                    self.map.remove(&old);
                }
                None => break,
            }
        }
        // Expired keys leave stale entries in `order`; keep it bounded.
        if self.order.len() > self.capacity * 2 {
            let map = &self.map;
            self.order.retain(|k| map.contains_key(k));
        }
    }
}

impl Page {
    /// Folds an oEmbed answer in. `prefer` lets it override the page's own title, image,
    /// author and site name; otherwise it only fills gaps.
    fn merge_embed(&mut self, e: Embed, prefer: bool) {
        let pick = |own: &mut Option<String>, theirs: &Option<String>| {
            if theirs.is_some() && (prefer || own.is_none()) {
                own.clone_from(theirs);
            }
        };
        pick(&mut self.title, &e.title);
        pick(&mut self.author, &e.author_name);
        pick(&mut self.site_name, &e.provider_name);
        if let Some(t) = &e.thumbnail {
            if prefer {
                self.images.retain(|m| m.url != t.url);
                self.images.insert(0, t.clone());
            } else if self.images.is_empty() {
                self.images.push(t.clone());
            }
        }
        if self.kind.is_none() {
            self.kind = match e.kind.as_str() {
                "video" => Some("video".into()),
                "photo" => Some("image".into()),
                _ => None,
            };
        }
        self.embed = Some(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keys_drop_fragments() {
        assert_eq!(cache_key("https://a.com/x#y").unwrap(), "https://a.com/x");
        assert_eq!(cache_key("ftp://a.com"), Err(Error::InvalidUrl));
    }

    #[test]
    fn cache_evicts_oldest() {
        let mut c = Cache::new(2);
        let p = Ok(Arc::new(Page::default()));
        let ttl = Duration::from_secs(60);
        c.put("a".into(), p.clone(), ttl, ttl);
        c.put("b".into(), p.clone(), ttl, ttl);
        c.put("c".into(), p, ttl, ttl);
        assert!(matches!(c.lookup("a"), Lookup::Miss));
        assert!(matches!(c.lookup("b"), Lookup::Fresh(_)));
        assert!(matches!(c.lookup("c"), Lookup::Fresh(_)));
    }

    #[test]
    fn recognises_bot_checks() {
        let page = |t: &str| Page {
            title: Some(t.into()),
            ..Default::default()
        };
        assert!(is_challenge(&page("Just a moment...")));
        assert!(is_challenge(&page("验证码_哔哩哔哩")));
        assert!(is_challenge(&page("Robot Check")));
        assert!(!is_challenge(&page(
            "Please wait for the launch: our new album"
        )));
        let mut real = page("Access denied: a novel");
        real.description = Some("A thriller.".into());
        assert!(!is_challenge(&real));
    }

    #[test]
    fn images_preview_themselves() {
        let p = other(
            Url::parse("https://a.com/cat.png").unwrap(),
            Some("image/png".into()),
        );
        assert_eq!(p.kind.as_deref(), Some("image"));
        assert_eq!(p.image().unwrap().url, "https://a.com/cat.png");
    }
}
