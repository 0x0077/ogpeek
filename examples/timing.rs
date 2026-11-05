//! Times each stage: building the client, a cold peek, a warm peek on the same host, a
//! cache hit. `cargo run --release --example timing -- <url> <another url on the same host>`

use std::time::Instant;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (a, b) = (&args[0], &args[1]);
    let t = Instant::now();
    let p = ogpeek::Peeker::new();
    println!("build      {:>8.1} ms", t.elapsed().as_secs_f64() * 1e3);
    if let Some(w) = args.get(2) {
        let t = Instant::now();
        let _ = p.peek(w).await;
        println!(
            "warm-up    {:>8.1} ms  (another host, loads TLS roots)",
            t.elapsed().as_secs_f64() * 1e3
        );
    }
    let t = Instant::now();
    let r = p.peek(a).await;
    println!(
        "cold       {:>8.1} ms  {:?}",
        t.elapsed().as_secs_f64() * 1e3,
        r.as_ref().map(|p| p.title.clone())
    );
    let t = Instant::now();
    let r = p.peek(b).await;
    println!(
        "warm       {:>8.1} ms  {:?}",
        t.elapsed().as_secs_f64() * 1e3,
        r.as_ref().map(|p| p.title.clone())
    );
    let t = Instant::now();
    let _ = p.peek(a).await;
    println!("cache hit  {:>8.3} ms", t.elapsed().as_secs_f64() * 1e3);
    let t = Instant::now();
    let f = p.fetch(b, "text/html", 8 << 20).await;
    println!(
        "full body  {:>8.1} ms  {} bytes",
        t.elapsed().as_secs_f64() * 1e3,
        f.map(|f| f.body.len()).unwrap_or(0)
    );
}
