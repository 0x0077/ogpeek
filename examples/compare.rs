//! For each URL: download the whole page, then compare what the head alone gives with
//! what the whole document gives. Also notes JSON-LD and whether the page is mostly script.

#[tokio::main]
async fn main() {
    let urls: Vec<String> = std::env::args().skip(1).collect();
    let p = ogpeek::Peeker::builder()
        .timeout(std::time::Duration::from_secs(20))
        .max_body_bytes(8 << 20)
        .build()
        .unwrap();
    let jobs: Vec<_> = urls
        .into_iter()
        .map(|u| {
            let p = p.clone();
            tokio::spawn(async move {
                let f = match p
                    .fetch(&u, "text/html,application/xhtml+xml", 8 << 20)
                    .await
                {
                    Ok(f) => f,
                    Err(e) => return format!("{u}\tERR {e}"),
                };
                let html = ogpeek::decode_html(&f.body, f.content_type.as_deref());
                let head = ogpeek::parse(&html, f.url.as_str());
                let all = ogpeek::parse_document(&html, f.url.as_str());
                let cut = html
                    .to_ascii_lowercase()
                    .find("</head")
                    .unwrap_or(html.len());
                let ld = html.matches("application/ld+json").count();
                let mark = |h: bool, a: bool| match (h, a) {
                    (true, _) => "Y",
                    (false, true) => "BODY",
                    _ => "-",
                };
                format!(
                    "{u}\tsize={}k head={}k\ttitle={} desc={} img={} icon={}\tld+json={ld}\t{}",
                    html.len() / 1024,
                    cut / 1024,
                    mark(head.title.is_some(), all.title.is_some()),
                    mark(head.description.is_some(), all.description.is_some()),
                    mark(!head.images.is_empty(), !all.images.is_empty()),
                    mark(!head.icons.is_empty(), !all.icons.is_empty()),
                    head.title
                        .as_deref()
                        .unwrap_or("")
                        .chars()
                        .take(30)
                        .collect::<String>()
                )
            })
        })
        .collect();
    for j in jobs {
        println!("{}", j.await.unwrap());
    }
}
