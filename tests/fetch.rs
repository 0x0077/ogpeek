//! Fetching against a local server. `allow_private` is on so 127.0.0.1 is reachable.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// How many times each path has been asked, across all tests (paths are unique per test).
static SEEN: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, usize>>> =
    std::sync::LazyLock::new(Default::default);

/// Serves each request with `answer(path, nth time this path was asked)`. A head of
/// `HANG` never answers. Returns the base URL and a request counter.
async fn serve(answer: fn(&str, usize) -> (String, Vec<u8>, bool)) -> (String, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            let h = h.clone();
            tokio::spawn(async move {
                let mut buf = vec![0; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_owned();
                h.fetch_add(1, Ordering::SeqCst);
                let nth = {
                    let mut seen = SEEN.lock().unwrap();
                    let n = seen.entry(path.clone()).or_default();
                    *n += 1;
                    *n
                };
                tokio::time::sleep(Duration::from_millis(50)).await;
                let (head, body, hang) = answer(&path, nth);
                if head == "HANG" {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    return;
                }
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(&body).await;
                if hang {
                    // Keeps the connection open with the rest of the page "still coming".
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
            });
        }
    });
    (base, hits)
}

fn ok(ct: &str, body: &str) -> (String, Vec<u8>, bool) {
    (
        format!("HTTP/1.1 200 OK\r\ncontent-type: {ct}\r\nconnection: close\r\n\r\n"),
        body.as_bytes().to_vec(),
        false,
    )
}

fn status(code: &str) -> (String, Vec<u8>, bool) {
    (
        format!("HTTP/1.1 {code}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"),
        vec![],
        false,
    )
}

fn routes(path: &str, nth: usize) -> (String, Vec<u8>, bool) {
    match path {
        "/hang" | "/abandoned" => ("HANG".into(), vec![], false),
        "/flaky" if nth == 1 => status("503 Service Unavailable"),
        "/goes-down" if nth > 1 => status("500 Internal Server Error"),
        "/goes-down" => ok("text/html", "<head><title>Up</title></head>"),
        "/slow" => {
            // Sends the head, then never finishes the body.
            let (h, b, _) = ok(
                "text/html",
                "<html><head><meta property=og:title content=Fast></head><body>",
            );
            (
                h.replace("connection: close\r\n", "transfer-encoding: identity\r\n"),
                b,
                true,
            )
        }
        "/early" => {
            // All the tags up front, then a long head that never ends.
            let mut b = String::from(
                "<html><head><title>T</title><meta name=description content=D>\
                <meta property=og:image content=/i.png><link rel=icon href=/f.png><script>",
            );
            b.push_str(&"x".repeat(40_000));
            let (h, _, _) = ok("text/html", "");
            (h.replace("connection: close\r\n", ""), b.into_bytes(), true)
        }
        "/video" => ok(
            "text/html",
            "<head><title>Loading</title><link rel=alternate type=application/json+oembed href=/oembed.json></head>",
        ),
        "/oembed.json" => ok(
            "application/json",
            r#"{"type":"video","title":"Real title","author_name":"Ana","thumbnail_url":"http://127.0.0.1/t.jpg","html":"<iframe></iframe>"}"#,
        ),
        "/moved" => (
            "HTTP/1.1 301 Moved\r\nlocation: /page\r\nconnection: close\r\n\r\n".into(),
            vec![],
            false,
        ),
        "/loop" => (
            "HTTP/1.1 302 Found\r\nlocation: /loop\r\nconnection: close\r\n\r\n".into(),
            vec![],
            false,
        ),
        "/page" => ok(
            "text/html; charset=utf-8",
            "<head><title>Page</title></head>",
        ),
        "/img.png" => ok("image/png", "\u{89}PNG"),
        "/bodymeta" => ok(
            "text/html",
            "<html><head></head><body><meta property=og:title content=Late></body>",
        ),
        "/refresh" => ok(
            "text/html",
            "<head><meta http-equiv=refresh content=\"0;url=/page\"></head>",
        ),
        "/missing" => (
            "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
            vec![],
            false,
        ),
        _ => ok("text/html", "<head><title>Other</title></head>"),
    }
}

fn peeker() -> ogpeek::Peeker {
    ogpeek::Peeker::builder()
        .allow_private(true)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

#[tokio::test]
async fn stops_reading_at_the_end_of_head() {
    let (base, _) = serve(routes).await;
    let t = Instant::now();
    let p = peeker().peek(&format!("{base}/slow")).await.unwrap();
    assert_eq!(p.title.as_deref(), Some("Fast"));
    assert!(
        t.elapsed() < Duration::from_secs(2),
        "took {:?}",
        t.elapsed()
    );
}

#[tokio::test]
async fn stops_once_the_preview_is_complete() {
    let (base, _) = serve(routes).await;
    let t = Instant::now();
    let p = peeker().peek(&format!("{base}/early")).await.unwrap();
    assert_eq!(p.title.as_deref(), Some("T"));
    assert_eq!(p.favicon.as_deref(), Some(format!("{base}/f.png").as_str()));
    assert!(
        t.elapsed() < Duration::from_secs(2),
        "took {:?}",
        t.elapsed()
    );
}

#[tokio::test]
async fn asks_advertised_oembed_when_the_page_lacks_an_image() {
    let (base, _) = serve(routes).await;
    let p = peeker().peek(&format!("{base}/video")).await.unwrap();
    assert_eq!(p.title.as_deref(), Some("Loading"));
    assert_eq!(p.image().unwrap().url, "http://127.0.0.1/t.jpg");
    assert_eq!(p.author.as_deref(), Some("Ana"));
    assert_eq!(p.kind.as_deref(), Some("video"));
    assert!(p.embed.as_ref().unwrap().html.is_some());
}

#[tokio::test]
async fn follows_redirects_and_refresh() {
    let (base, _) = serve(routes).await;
    let pk = peeker();
    let p = pk.peek(&format!("{base}/moved")).await.unwrap();
    assert_eq!(p.title.as_deref(), Some("Page"));
    assert!(p.url.ends_with("/page"));
    assert_eq!(
        pk.peek(&format!("{base}/loop")).await.unwrap_err(),
        ogpeek::Error::TooManyRedirects
    );
    let p = pk.peek(&format!("{base}/refresh")).await.unwrap();
    assert_eq!(p.title.as_deref(), Some("Page"));
}

#[tokio::test]
async fn images_statuses_and_body_meta() {
    let (base, _) = serve(routes).await;
    let pk = peeker();
    let p = pk.peek(&format!("{base}/img.png")).await.unwrap();
    assert_eq!(p.kind.as_deref(), Some("image"));
    assert_eq!(
        pk.peek(&format!("{base}/missing")).await.unwrap_err(),
        ogpeek::Error::Status(404)
    );
    let p = pk.peek(&format!("{base}/bodymeta")).await.unwrap();
    assert_eq!(p.title.as_deref(), Some("Late"));
}

#[tokio::test]
async fn concurrent_requests_share_one_fetch_and_cache() {
    let (base, hits) = serve(routes).await;
    let pk = peeker();
    let url = format!("{base}/page");
    let all = futures_join(&pk, &url, 20).await;
    assert!(all.iter().all(|p| p.title.as_deref() == Some("Page")));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let t = Instant::now();
    pk.peek(&url).await.unwrap();
    assert!(t.elapsed() < Duration::from_millis(5));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(pk.cached(&url).is_some());
}

async fn futures_join(pk: &ogpeek::Peeker, url: &str, n: usize) -> Vec<Arc<ogpeek::Page>> {
    let tasks: Vec<_> = (0..n)
        .map(|_| {
            let pk = pk.clone();
            let url = url.to_owned();
            tokio::spawn(async move { pk.peek(&url).await.unwrap() })
        })
        .collect();
    let mut out = Vec::new();
    for t in tasks {
        out.push(t.await.unwrap());
    }
    out
}

#[tokio::test]
async fn refuses_internal_addresses_by_default() {
    let pk = ogpeek::Peeker::new();
    for url in [
        "http://127.0.0.1/",
        "http://[::1]/",
        "http://169.254.169.254/latest/meta-data/",
        "http://10.0.0.1/",
        "http://localhost/",
        "https://example.com:8443/",
    ] {
        assert_eq!(
            pk.peek(url).await.unwrap_err(),
            ogpeek::Error::Blocked,
            "{url}"
        );
    }
    assert_eq!(
        pk.peek("file:///etc/passwd").await.unwrap_err(),
        ogpeek::Error::InvalidUrl
    );
    assert_eq!(
        pk.peek("http://user:pw@example.com/").await.unwrap_err(),
        ogpeek::Error::InvalidUrl
    );
}

#[tokio::test]
async fn a_peek_never_outlives_its_timeout() {
    let (base, _) = serve(routes).await;
    let pk = ogpeek::Peeker::builder()
        .allow_private(true)
        .timeout(Duration::from_millis(800))
        .build()
        .unwrap();
    let t = Instant::now();
    assert_eq!(
        pk.peek(&format!("{base}/hang")).await.unwrap_err(),
        ogpeek::Error::Timeout
    );
    assert!(
        t.elapsed() < Duration::from_millis(1300),
        "took {:?}",
        t.elapsed()
    );
}

#[tokio::test]
async fn retries_once_on_503() {
    let (base, hits) = serve(routes).await;
    let pk = peeker();
    let p = pk.peek(&format!("{base}/flaky")).await.unwrap();
    assert_eq!(p.title.as_deref(), Some("Other"));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert_eq!(pk.stats().retries, 1);
}

#[tokio::test]
async fn serves_stale_while_refreshing_and_when_the_site_is_down() {
    let (base, _) = serve(routes).await;
    let pk = ogpeek::Peeker::builder()
        .allow_private(true)
        .retry(false)
        .cache_ttl(Duration::from_millis(100))
        .stale_ttl(Duration::from_secs(60))
        .error_ttl(Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("{base}/goes-down");
    assert_eq!(pk.peek(&url).await.unwrap().title.as_deref(), Some("Up"));
    tokio::time::sleep(Duration::from_millis(150)).await;
    // Expired: answered at once from the old copy while a refresh runs.
    let t = Instant::now();
    assert_eq!(pk.peek(&url).await.unwrap().title.as_deref(), Some("Up"));
    assert!(t.elapsed() < Duration::from_millis(20));
    tokio::time::sleep(Duration::from_millis(400)).await;
    let s = pk.stats();
    assert_eq!((s.stale_hits, s.errors, s.stale_on_error), (1, 1, 1));
    // The refresh failed: the old copy is kept, and the dead site is not asked again yet.
    assert_eq!(pk.peek(&url).await.unwrap().title.as_deref(), Some("Up"));
    assert_eq!(pk.stats().fetches, 2);
    assert!(pk.peek_fresh(&url).await.is_ok());
}

#[tokio::test]
async fn abandoned_requests_leave_nothing_behind() {
    let (base, _) = serve(routes).await;
    let pk = peeker();
    let r = tokio::time::timeout(
        Duration::from_millis(200),
        pk.peek(&format!("{base}/abandoned")),
    )
    .await;
    assert!(r.is_err());
    assert_eq!(pk.stats().in_flight, 0);
}
