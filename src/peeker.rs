//! Fetching pages: the client, the cache, and request coalescing.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE, HeaderName, HeaderValue, LOCATION};
use tokio::sync::OnceCell;
use url::Url;

use crate::net::{Error, Resolver, forbidden_ip};
use crate::page::{Media, Page};
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
    cache_capacity: usize,
    accept_language: Option<String>,
    follow_refresh: bool,
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
            max_head_bytes: 512 << 10,
            max_body_bytes: 2 << 20,
            max_redirects: 5,
            allow_private: false,
            any_port: false,
            cache_ttl: Duration::from_secs(600),
            error_ttl: Duration::from_secs(30),
            cache_capacity: 4096,
            accept_language: None,
            follow_refresh: true,
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
    /// Limit for one whole request, body included. Default 8 seconds.
    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }
    /// Limit for opening a connection. Default 3 seconds.
    pub fn connect_timeout(mut self, t: Duration) -> Self {
        self.connect_timeout = t;
        self
    }
    /// Most bytes read while looking for the end of `<head>`. Default 512 KiB.
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
    /// Concurrent calls for the same URL share one request.
    pub async fn peek(&self, url: &str) -> Result<Arc<Page>, Error> {
        let key = cache_key(url)?;
        if let Some(hit) = self.inner.cache.lock().expect("cache").get(&key) {
            return hit;
        }
        self.coalesced(key).await
    }

    /// Fetches `url` even when a cached copy exists, and caches the result.
    pub async fn peek_fresh(&self, url: &str) -> Result<Arc<Page>, Error> {
        let key = cache_key(url)?;
        self.coalesced(key).await
    }

    /// The cached preview for `url`, if there is a fresh one. Never fetches.
    pub fn cached(&self, url: &str) -> Option<Arc<Page>> {
        let key = cache_key(url).ok()?;
        self.inner.cache.lock().expect("cache").get(&key)?.ok()
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
        let result = cell
            .get_or_init(|| async { self.load(&key).await.map(Arc::new) })
            .await
            .clone();
        let mut flights = self.inner.flights.lock().expect("flights");
        if flights.get(&key).is_some_and(|c| Arc::ptr_eq(c, &cell)) {
            flights.remove(&key);
            let ttl = if result.is_ok() {
                self.inner.opts.cache_ttl
            } else {
                self.inner.opts.error_ttl
            };
            self.inner
                .cache
                .lock()
                .expect("cache")
                .put(key, result.clone(), ttl);
        }
        result
    }

    async fn load(&self, start: &str) -> Result<Page, Error> {
        let opts = &self.inner.opts;
        let mut target = start.to_owned();
        for hop in 0..2 {
            let (url, mut res) = self.open(&target, ACCEPT_HTML).await?;
            let ct = content_type(&res);
            if !is_html(ct.as_deref()) {
                return Ok(other(url, ct));
            }
            let mut body = Vec::with_capacity(16 << 10);
            let mut stopped = false;
            while let Some(chunk) = res.chunk().await? {
                let from = body.len().saturating_sub(6);
                body.extend_from_slice(&chunk);
                if head_end(&body[from..]).is_some() {
                    stopped = true;
                    break;
                }
                if body.len() >= opts.max_head_bytes {
                    stopped = true;
                    break;
                }
            }
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

    /// Fetches an HTML page only up to the end of its `<head>` (at most the configured
    /// `max_head_bytes`).
    pub async fn fetch_head(&self, url: &str) -> Result<Fetched, Error> {
        let (url, mut res) = self.open(url, ACCEPT_HTML).await?;
        let content_type = content_type(&res);
        let mut body = Vec::new();
        let mut partial = false;
        while let Some(chunk) = res.chunk().await? {
            let from = body.len().saturating_sub(6);
            body.extend_from_slice(&chunk);
            if head_end(&body[from..]).is_some() || body.len() >= self.inner.opts.max_head_bytes {
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
            let res = req.send().await?;
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
    map: HashMap<String, (Instant, Result<Arc<Page>, Error>)>,
    order: VecDeque<String>,
}

impl Cache {
    fn new(capacity: usize) -> Self {
        Cache {
            capacity,
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&mut self, key: &str) -> Option<Result<Arc<Page>, Error>> {
        match self.map.get(key) {
            Some((until, r)) if Instant::now() < *until => Some(r.clone()),
            Some(_) => {
                self.map.remove(key);
                None
            }
            None => None,
        }
    }

    fn put(&mut self, key: String, value: Result<Arc<Page>, Error>, ttl: Duration) {
        if self.capacity == 0 || ttl.is_zero() {
            return;
        }
        if self
            .map
            .insert(key.clone(), (Instant::now() + ttl, value))
            .is_none()
        {
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
        c.put("a".into(), p.clone(), ttl);
        c.put("b".into(), p.clone(), ttl);
        c.put("c".into(), p, ttl);
        assert!(c.get("a").is_none());
        assert!(c.get("b").is_some() && c.get("c").is_some());
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
