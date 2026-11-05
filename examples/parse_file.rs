//! `cargo run --example parse_file -- page.html https://page.url` prints what ogpeek reads.
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let bytes = std::fs::read(&a[0]).expect("read");
    let p = ogpeek::parse_bytes(
        &bytes,
        a.get(1).map_or("https://example.com/", String::as_str),
        None,
    );
    println!("{p:#?}");
}
