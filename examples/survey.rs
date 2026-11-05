//! Peeks many URLs at once and prints what each preview would show, then the totals.
//! `cargo run --release --example survey -- <url>...`

use std::time::Instant;

#[tokio::main]
async fn main() {
    let urls: Vec<String> = std::env::args().skip(1).collect();
    let pk = ogpeek::Peeker::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let started = Instant::now();
    let jobs: Vec<_> = urls
        .into_iter()
        .map(|u| {
            let pk = pk.clone();
            tokio::spawn(async move {
                let t = Instant::now();
                let r = pk.peek(&u).await;
                (u, r, t.elapsed().as_millis())
            })
        })
        .collect();
    let (mut full, mut total) = (0, 0);
    for j in jobs {
        let (u, r, ms) = j.await.unwrap();
        total += 1;
        match r {
            Ok(p) => {
                let has = |b: bool, c: char| if b { c } else { '.' };
                let card = p.title.is_some() && p.image().is_some();
                full += usize::from(card);
                println!(
                    "{ms:>6} ms {}{}{}{}{} {:<48} {}",
                    has(p.title.is_some(), 'T'),
                    has(p.description.is_some(), 'D'),
                    has(p.image().is_some(), 'I'),
                    has(p.schema.is_some(), 'S'),
                    has(p.embed.is_some(), 'E'),
                    u.chars().take(48).collect::<String>(),
                    p.title
                        .as_deref()
                        .unwrap_or("")
                        .chars()
                        .take(40)
                        .collect::<String>()
                );
            }
            Err(e) => println!(
                "{ms:>6} ms ----- {:<48} {e}",
                u.chars().take(48).collect::<String>()
            ),
        }
    }
    println!(
        "\n{full}/{total} with title and image, {} ms in all, {:?}",
        started.elapsed().as_millis(),
        pk.stats()
    );
}
