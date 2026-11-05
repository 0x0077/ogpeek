use ogpeek::{parse, parse_bytes, parse_document};

const URL: &str = "https://example.com/blog/post?id=1";

#[test]
fn open_graph_wins_over_fallbacks() {
    let p = parse(
        r#"<html lang="pt-BR"><head>
        <title>Tag title</title>
        <meta name="twitter:title" content="Twitter title">
        <meta property="og:title" content="OG title">
        <meta name="description" content="Plain description">
        <meta property="og:site_name" content="Example">
        <meta property="og:type" content="article">
        </head>"#,
        URL,
    );
    assert_eq!(p.title.as_deref(), Some("OG title"));
    assert_eq!(p.description.as_deref(), Some("Plain description"));
    assert_eq!(p.site_name.as_deref(), Some("Example"));
    assert_eq!(p.kind.as_deref(), Some("article"));
    assert_eq!(p.locale.as_deref(), Some("pt-BR"));
    assert_eq!(p.url, URL);
}

#[test]
fn title_tag_and_twitter_fallbacks() {
    let p = parse(
        "<head><title> Hello\n  &amp; bye </title><meta name=twitter:image content=/a.png></head>",
        URL,
    );
    assert_eq!(p.title.as_deref(), Some("Hello & bye"));
    assert_eq!(p.image().unwrap().url, "https://example.com/a.png");
    assert_eq!(
        p.favicon.as_deref(),
        Some("https://example.com/favicon.ico")
    );
}

#[test]
fn structured_images() {
    let p = parse(
        r#"<meta property="og:image" content="http://cdn.example.com/1.jpg">
        <meta property="og:image:secure_url" content="https://cdn.example.com/1.jpg">
        <meta property="og:image:width" content="1200"><meta property="og:image:height" content="630">
        <meta property="og:image:alt" content="First">
        <meta property="og:image" content="//cdn.example.com/2.jpg">
        <meta property="og:image:width" content="600">
        <meta property="og:image" content="javascript:alert(1)">"#,
        URL,
    );
    assert_eq!(p.images.len(), 2);
    assert_eq!(p.images[0].url, "https://cdn.example.com/1.jpg");
    assert_eq!(
        (p.images[0].width, p.images[0].height),
        (Some(1200), Some(630))
    );
    assert_eq!(p.images[0].alt.as_deref(), Some("First"));
    assert_eq!(p.images[1].url, "https://cdn.example.com/2.jpg");
    assert_eq!(p.images[1].width, Some(600));
}

#[test]
fn ignores_scripts_comments_and_body() {
    let p = parse(
        r#"<head>
        <!-- <meta property="og:title" content="in a comment"> -->
        <script>var s = '<meta property="og:title" content="in a script">';</script>
        <style>/* <title>in a style</title> */</style>
        <meta property="og:description" content="real">
        </head><body><meta property="og:title" content="in the body"></body>"#,
        URL,
    );
    assert_eq!(p.title, None);
    assert_eq!(p.description.as_deref(), Some("real"));
    let whole = parse_document(
        r#"<body><meta property="og:title" content="in the body"></body>"#,
        URL,
    );
    assert_eq!(whole.title.as_deref(), Some("in the body"));
}

#[test]
fn attribute_forms() {
    let p = parse(
        "<META PROPERTY='og:title' CONTENT='Single'><meta name=description content=unquoted>\
         <meta content=\"after\" property=\"og:site_name\"/>",
        URL,
    );
    assert_eq!(p.title.as_deref(), Some("Single"));
    assert_eq!(p.description.as_deref(), Some("unquoted"));
    assert_eq!(p.site_name.as_deref(), Some("after"));
}

#[test]
fn links_icons_feeds_oembed_base() {
    let p = parse(
        r##"<base href="https://static.example.org/root/">
        <link rel="icon" href="fav-16.png" sizes="16x16">
        <link rel="apple-touch-icon" href="/touch.png" sizes="180x180">
        <link rel="mask-icon" href="mask.svg" sizes="any">
        <link rel="canonical" href="https://example.com/blog/post">
        <link rel="alternate" type="application/rss+xml" title="Posts" href="feed.xml">
        <link rel="alternate" type="application/json+oembed" href="/oembed?url=x&amp;format=json">
        <meta name="keywords" content="rust, html , , previews">
        <meta name="theme-color" content="#F2661B">"##,
        URL,
    );
    assert_eq!(p.icons.len(), 3);
    assert_eq!(
        p.favicon.as_deref(),
        Some("https://static.example.org/touch.png")
    );
    assert_eq!(
        p.canonical.as_deref(),
        Some("https://example.com/blog/post")
    );
    assert_eq!(p.feeds[0].url, "https://static.example.org/root/feed.xml");
    assert_eq!(p.feeds[0].title.as_deref(), Some("Posts"));
    assert_eq!(
        p.oembed.as_deref(),
        Some("https://static.example.org/oembed?url=x&format=json")
    );
    assert_eq!(p.keywords, ["rust", "html", "previews"]);
    assert_eq!(p.theme_color.as_deref(), Some("#F2661B"));
}

#[test]
fn microdata_and_refresh() {
    let p = parse(
        r#"<meta itemprop="name" content="Video name"><meta itemprop="thumbnailUrl" content="/t.jpg">
        <meta http-equiv="refresh" content="0; URL='https://example.net/next'">"#,
        URL,
    );
    assert_eq!(p.title.as_deref(), Some("Video name"));
    assert_eq!(p.image().unwrap().url, "https://example.com/t.jpg");
    assert_eq!(p.refresh.as_deref(), Some("https://example.net/next"));
}

#[test]
fn legacy_encodings() {
    // "中文标题" in GBK, declared in a meta tag
    let mut html = b"<head><meta charset=\"gbk\"><title>".to_vec();
    html.extend_from_slice(&[0xD6, 0xD0, 0xCE, 0xC4, 0xB1, 0xEA, 0xCC, 0xE2]);
    html.extend_from_slice(b"</title></head>");
    assert_eq!(
        parse_bytes(&html, URL, None).title.as_deref(),
        Some("中文标题")
    );
    // "テスト" in Shift_JIS, declared by the HTTP header
    let mut html = b"<title>".to_vec();
    html.extend_from_slice(&[0x83, 0x65, 0x83, 0x58, 0x83, 0x67]);
    html.extend_from_slice(b"</title>");
    let p = parse_bytes(&html, URL, Some("text/html; charset=Shift_JIS"));
    assert_eq!(p.title.as_deref(), Some("テスト"));
    assert_eq!(
        p.content_type.as_deref(),
        Some("text/html; charset=Shift_JIS")
    );
}

#[test]
fn survives_broken_html() {
    for html in [
        "<",
        "<meta",
        "<meta content=\"x",
        "<!--",
        "<title>never closed",
        "<<<>>>",
        "</",
        "<a =b>",
        "<meta property=og:title content=é>",
    ] {
        let _ = parse(html, URL);
    }
    let p = parse("<meta property=og:title content=é>", URL);
    assert_eq!(p.title.as_deref(), Some("é"));
}
