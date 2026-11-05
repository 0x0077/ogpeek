//! Parses a realistic page head (about 50 meta and link tags, inline scripts and styles) and a
//! whole 300 KB page, with ogpeek and with a full HTML5 parser (scraper) reading the same
//! fields. Run with `cargo bench`.

use criterion::{Criterion, black_box, criterion_group, criterion_main};

fn head() -> String {
    let mut h = String::from("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    h.push_str("<title>Launch notes: what shipped this week &mdash; Example Journal</title>");
    h.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    h.push_str("<meta name=\"description\" content=\"A long description of the article that runs for a sentence or two, with an &amp; or two.\">");
    for (k, v) in [
        ("og:title", "Launch notes: what shipped this week"),
        ("og:description", "Everything that shipped, in one place."),
        ("og:type", "article"),
        ("og:site_name", "Example Journal"),
        ("og:url", "https://journal.example.com/2026/launch-notes"),
        (
            "og:image",
            "https://cdn.example.com/img/launch-notes-1200x630.jpg",
        ),
        ("og:image:width", "1200"),
        ("og:image:height", "630"),
        ("og:image:alt", "A rocket on a launch pad at dusk"),
        ("article:published_time", "2026-11-03T09:00:00Z"),
    ] {
        h.push_str(&format!("<meta property=\"{k}\" content=\"{v}\">"));
    }
    for (k, v) in [
        ("twitter:card", "summary_large_image"),
        ("twitter:site", "@example"),
        ("twitter:creator", "@writer"),
    ] {
        h.push_str(&format!("<meta name=\"{k}\" content=\"{v}\">"));
    }
    for i in 0..30 {
        h.push_str(&format!(
            "<link rel=\"preload\" href=\"/assets/chunk-{i}.js\" as=\"script\">"
        ));
    }
    h.push_str("<link rel=\"icon\" href=\"/favicon-32.png\" sizes=\"32x32\"><link rel=\"apple-touch-icon\" href=\"/apple-180.png\" sizes=\"180x180\">");
    h.push_str(
        "<link rel=\"alternate\" type=\"application/rss+xml\" title=\"Feed\" href=\"/feed.xml\">",
    );
    h.push_str("<script>window.__DATA__ = {\"items\": [");
    for i in 0..400 {
        h.push_str(&format!(
            "{{\"id\": {i}, \"html\": \"<meta property='og:title' content='fake'>\"}},"
        ));
    }
    h.push_str("]};</script><style>");
    for i in 0..300 {
        h.push_str(&format!(".c{i}{{color:#{i:03x};margin:{i}px}}"));
    }
    h.push_str("</style></head>");
    h
}

fn page() -> String {
    let mut p = head();
    p.push_str("<body><main>");
    while p.len() < 300_000 {
        p.push_str("<article><h2>Heading</h2><p>Some paragraph text with <a href=\"/x\">a link</a> and <em>emphasis</em>.</p></article>");
    }
    p.push_str("</main></body></html>");
    p
}

fn with_scraper(html: &str) -> (Option<String>, Option<String>, Option<String>) {
    let doc = scraper::Html::parse_document(html);
    let meta = |prop: &str| {
        let sel =
            scraper::Selector::parse(&format!("meta[property=\"{prop}\"], meta[name=\"{prop}\"]"))
                .unwrap();
        doc.select(&sel)
            .next()
            .and_then(|e| e.value().attr("content"))
            .map(str::to_owned)
    };
    (meta("og:title"), meta("og:description"), meta("og:image"))
}

fn bench(c: &mut Criterion) {
    let head = head();
    let page = page();
    let url = "https://journal.example.com/2026/launch-notes";
    let mut g = c.benchmark_group("head");
    g.bench_function("ogpeek", |b| {
        b.iter(|| ogpeek::parse(black_box(&head), url))
    });
    g.bench_function("scraper", |b| b.iter(|| with_scraper(black_box(&head))));
    g.finish();
    let mut g = c.benchmark_group("page_300k");
    g.bench_function("ogpeek", |b| {
        b.iter(|| ogpeek::parse(black_box(&page), url))
    });
    g.bench_function("scraper", |b| b.iter(|| with_scraper(black_box(&page))));
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
