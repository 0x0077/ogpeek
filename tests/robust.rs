//! The parser must never panic, whatever arrives: random bytes, HTML cut at every
//! position (as when a stream stops early), and random mixes of HTML fragments with
//! multibyte text.

use ogpeek::{clean_text, decode, parse, parse_bytes, parse_document, parse_oembed};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const PIECES: &[&str] = &[
    "<",
    ">",
    "</",
    "/>",
    "<!--",
    "-->",
    "<!doctype html>",
    "<html lang=",
    "<head>",
    "</head>",
    "<body>",
    "<meta ",
    "<link ",
    "<title>",
    "</title>",
    "<script>",
    "</script>",
    "<script type=\"application/ld+json\">",
    "<style>",
    "<svg>",
    "</svg>",
    "<base href=",
    "property=",
    "name=",
    "content=",
    "rel=",
    "href=",
    "sizes=",
    "type=",
    "\"",
    "'",
    "=",
    " ",
    "\n",
    "og:title",
    "og:image",
    "og:image:width",
    "og:image:secure_url",
    "twitter:image",
    "description",
    "icon",
    "apple-touch-icon",
    "alternate",
    "application/json+oembed",
    "application/rss+xml",
    "canonical",
    "http-equiv=refresh",
    "0;url=",
    "&amp;",
    "&#",
    "&#x",
    ";",
    "&nbsp",
    "{",
    "}",
    "[",
    "]",
    "\"@type\":",
    "\"Product\"",
    "\"@graph\":",
    "\"name\":",
    "\"offers\":",
    ":",
    ",",
    "中文标题",
    "日本語",
    "한국어",
    "é",
    "👩‍💻",
    "\u{202E}",
    "\0",
    "https://example.com/a.png",
    "//cdn.example.com/x",
    "javascript:alert(1)",
    "/rel",
    "99999999999",
    "x",
];

#[test]
fn random_fragments_never_panic() {
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..30_000 {
        let n = 1 + r.below(60);
        let doc: String = (0..n).map(|_| PIECES[r.below(PIECES.len())]).collect();
        let _ = parse(&doc, "https://example.com/page");
        let _ = parse_document(&doc, "not a url");
        let _ = decode(&doc);
        let _ = clean_text(&doc);
        let _ = parse_oembed(doc.as_bytes());
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut r = Rng(42);
    let charsets = [
        None,
        Some("text/html; charset=gbk"),
        Some("text/html; charset=shift_jis"),
        Some("charset=utf-16"),
    ];
    for _ in 0..5_000 {
        let len = r.below(2_000);
        let bytes: Vec<u8> = (0..len)
            .map(|_| {
                if r.below(4) == 0 {
                    b"<>=\"' /&;"[r.below(9)]
                } else {
                    r.next() as u8
                }
            })
            .collect();
        let _ = parse_bytes(
            &bytes,
            "https://example.com/",
            charsets[r.below(charsets.len())],
        );
        let _ = parse_oembed(&bytes);
    }
}

#[test]
fn every_cut_of_a_real_head_parses() {
    let doc = r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><title>标题 &amp; 副标题</title>
<meta property="og:title" content="开放图谱标题"><meta property="og:image" content="/封面.png">
<meta property="og:image:width" content="1200"><link rel="icon" href="/favicon.png" sizes="32x32">
<script type="application/ld+json">{"@type":"Article","headline":"文章","author":{"name":"作者"}}</script>
<script>var s = "<meta property='og:title' content='假的'>";</script><!-- 注释 --></head><body>正文</body></html>"#;
    for (i, _) in doc.char_indices() {
        let _ = parse(&doc[..i], "https://example.com/");
        let _ = parse_bytes(&doc.as_bytes()[..i], "https://example.com/", None);
    }
    for i in 0..doc.len() {
        let _ = parse_bytes(&doc.as_bytes()[..i], "https://example.com/", None);
    }
    let p = parse(doc, "https://example.com/");
    assert_eq!(p.title.as_deref(), Some("开放图谱标题"));
    assert_eq!(
        p.image().unwrap().url,
        "https://example.com/%E5%B0%81%E9%9D%A2.png"
    );
}
