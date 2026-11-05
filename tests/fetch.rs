//! Fetching against a local server. `allow_private` is on so 127.0.0.1 is reachable.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serves each request with `answer(path)`. Returns the base URL and a request counter.
async fn serve(answer: fn(&str) -> (String, Vec<u8>, bool)) -> (String, Arc<AtomicUsize>) {
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
                tokio::time::sleep(Duration::from_millis(50)).await;
                let (head, body, hang) = answer(&path);
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

fn routes(path: &str) -> (String, Vec<u8>, bool) {
    match path {
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
