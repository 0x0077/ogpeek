//! `ogpeek <url>...` prints each page's preview as JSON, with how long it took.

use std::time::Instant;

#[tokio::main]
async fn main() {
    let urls: Vec<String> = std::env::args().skip(1).collect();
    if urls.is_empty() || urls.iter().any(|a| a == "-h" || a == "--help") {
        eprintln!(
            "usage: ogpeek <url>...\n\nPrints each page's preview as JSON. Several URLs are fetched at once."
        );
        std::process::exit(2);
    }
    let peeker = ogpeek::Peeker::new();
    let jobs = urls.into_iter().map(|u| {
        let p = peeker.clone();
        tokio::spawn(async move {
            let t = Instant::now();
            let r = p.peek(&u).await;
            (u, r, t.elapsed())
        })
    });
    let mut failed = false;
    for job in jobs.collect::<Vec<_>>() {
        let (url, result, took) = job.await.expect("task");
        let ms = took.as_secs_f64() * 1000.0;
        let out = match result {
            Ok(page) => serde_json::json!({ "ms": (ms * 10.0).round() / 10.0, "page": &*page }),
            Err(e) => {
                failed = true;
                serde_json::json!({ "url": url, "ms": (ms * 10.0).round() / 10.0, "error": e.to_string() })
            }
        };
        println!("{}", serde_json::to_string_pretty(&out).expect("json"));
    }
    if failed {
        std::process::exit(1);
    }
}
