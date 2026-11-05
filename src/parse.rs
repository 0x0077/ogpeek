//! A single-pass scanner for the parts of HTML that describe a page.
//!
//! It is not a general HTML parser. It walks the document once, looks only at `<meta>`,
//! `<link>`, `<base>`, `<html>` and `<title>`, steps over comments and the raw text of
//! `<script>`, `<style>`, `<template>` and `<textarea>` (so a `<meta>` inside a script string
//! is never read), and stops at `</head>` or `<body>`. Nothing is allocated for tags it
//! does not keep.

use std::borrow::Cow;

use encoding_rs::{Encoding, UTF_8};
use memchr::{memchr, memmem};
use url::Url;

use crate::entities::decode;
use crate::page::{Feed, Icon, Media, Page, Twitter};

const MAX_IMAGES: usize = 16;
const MAX_ICONS: usize = 16;
const MAX_FEEDS: usize = 8;
const MAX_KEYWORDS: usize = 24;
const MAX_ATTRS: usize = 32;

/// Reads a page's metadata from HTML text.
///
/// `url` is the page's address; relative links are resolved against it (or against
/// `<base href>` when the page sets one). Parsing stops at `</head>` or `<body>`.
///
/// ```
/// let html = r#"<head><meta property="og:title" content="Hello &amp; welcome">
///   <link rel="icon" href="/favicon.png" sizes="64x64"></head>"#;
/// let page = ogpeek::parse(html, "https://example.com/post");
/// assert_eq!(page.title.as_deref(), Some("Hello & welcome"));
/// assert_eq!(page.favicon.as_deref(), Some("https://example.com/favicon.png"));
/// ```
pub fn parse(html: &str, url: &str) -> Page {
    scan(html, url, true)
}

/// Like [`parse`], but keeps reading past `</head>` to the end of the document. Some pages
/// put their `<meta>` tags in the body.
pub fn parse_document(html: &str, url: &str) -> Page {
    scan(html, url, false)
}

/// Reads a page's metadata from raw bytes, working out the character encoding first.
///
/// The encoding comes from a byte order mark, then the `charset` in `content_type`
/// (the HTTP `Content-Type` header), then a `<meta charset>` near the top of the page,
/// and is UTF-8 otherwise. GBK, Shift_JIS, EUC-KR, Windows-1251 and the rest of the
/// WHATWG encodings are supported.
pub fn parse_bytes(bytes: &[u8], url: &str, content_type: Option<&str>) -> Page {
    let mut page = parse(&decode_html(bytes, content_type), url);
    page.content_type = content_type.map(str::to_owned);
    page
}

/// Decodes HTML bytes to text using the rules described in [`parse_bytes`].
pub fn decode_html<'a>(bytes: &'a [u8], content_type: Option<&str>) -> Cow<'a, str> {
    let enc = Encoding::for_bom(bytes)
        .map(|(e, _)| e)
        .or_else(|| {
            content_type
                .and_then(header_charset)
                .and_then(Encoding::for_label)
        })
        .or_else(|| meta_charset(&bytes[..bytes.len().min(4096)]))
        .unwrap_or(UTF_8);
    if enc == UTF_8 {
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
        return String::from_utf8_lossy(bytes);
    }
    enc.decode(bytes).0
}

fn header_charset(ct: &str) -> Option<&[u8]> {
    let i = find_ci(ct.as_bytes(), b"charset=")?;
    let v = ct[i + 8..].trim_start().trim_start_matches(['"', '\'']);
    let end = v
        .find(|c: char| c == ';' || c == '"' || c == '\'' || c.is_whitespace())
        .unwrap_or(v.len());
    Some(&v.as_bytes()[..end])
}

fn meta_charset(prefix: &[u8]) -> Option<&'static Encoding> {
    let mut from = 0;
    while let Some(i) = find_ci(&prefix[from..], b"charset") {
        let mut j = from + i + 7;
        from = j;
        while j < prefix.len() && prefix[j].is_ascii_whitespace() {
            j += 1;
        }
        if prefix.get(j) != Some(&b'=') {
            continue;
        }
        j += 1;
        while j < prefix.len()
            && (prefix[j].is_ascii_whitespace() || prefix[j] == b'"' || prefix[j] == b'\'')
        {
            j += 1;
        }
        let end = j + prefix[j..]
            .iter()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b':'))
            .count();
        // A page that says it is UTF-16 in an ASCII meta tag is not; browsers read it as UTF-8.
        if let Some(e) = Encoding::for_label(&prefix[j..end]) {
            return Some(e.output_encoding());
        }
    }
    None
}

/// Position of `needle` in `hay`, ignoring ASCII case. `needle` must be lowercase.
pub(crate) fn find_ci(hay: &[u8], needle: &[u8]) -> Option<usize> {
    let (&first, rest) = needle.split_first()?;
    let upper = first.to_ascii_uppercase();
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        let off = memchr::memchr2(first, upper, &hay[i..=hay.len() - needle.len()])?;
        let at = i + off;
        if hay[at + 1..at + needle.len()].eq_ignore_ascii_case(rest) {
            return Some(at);
        }
        i = at + 1;
    }
    None
}

/// Where the useful part of a document ends: the start of `</head` or `<body`, if seen.
#[cfg_attr(not(feature = "fetch"), allow(dead_code))]
pub(crate) fn head_end(hay: &[u8]) -> Option<usize> {
    let mut i = 0;
    while let Some(off) = memchr(b'<', &hay[i..]) {
        let at = i + off;
        let r = &hay[at + 1..];
        if (r.len() >= 5 && r[..5].eq_ignore_ascii_case(b"/head"))
            || (r.len() >= 4 && r[..4].eq_ignore_ascii_case(b"body"))
        {
            return Some(at);
        }
        i = at + 1;
    }
    None
}

fn scan(html: &str, url: &str, head_only: bool) -> Page {
    let b = html.as_bytes();
    let mut c = Collector::new(url);
    let mut attrs: Vec<(&str, &str)> = Vec::with_capacity(8);
    let mut i = 0;
    while let Some(off) = memchr(b'<', &b[i..]) {
        let p = i + off;
        if b[p + 1..].starts_with(b"!--") {
            match memmem::find(&b[p + 4..], b"-->") {
                Some(e) => {
                    i = p + 4 + e + 3;
                    continue;
                }
                None => break,
            }
        }
        let closing = b.get(p + 1) == Some(&b'/');
        let ns = p + 1 + usize::from(closing);
        let ne = ns
            + b[ns..]
                .iter()
                .take_while(|c| c.is_ascii_alphanumeric() || **c == b'-' || **c == b':')
                .count();
        if ne == ns || !b[ns].is_ascii_alphabetic() {
            i = p + 1;
            continue;
        }
        let name = &html[ns..ne];
        attrs.clear();
        let end = read_attrs(b, html, ne, &mut attrs);
        if closing {
            if head_only && name.eq_ignore_ascii_case("head") {
                break;
            }
            i = end;
            continue;
        }
        let is = |n: &str| name.eq_ignore_ascii_case(n);
        if is("meta") {
            c.meta(&attrs);
        } else if is("link") {
            c.link(&attrs);
        } else if is("title") {
            let (text, next) = raw_text(b, html, end, b"title");
            if c.title_tag.is_none() {
                c.title_tag = some(decode(text));
            }
            i = next;
            continue;
        } else if is("script") || is("style") || is("template") || is("textarea") {
            i = raw_text(b, html, end, name.as_bytes()).1;
            continue;
        } else if is("base") {
            c.base(&attrs);
        } else if is("html") {
            if let Some(lang) = attr(&attrs, "lang") {
                c.html_lang = some(decode(lang));
            }
        } else if head_only && is("body") {
            break;
        } else if is("svg") {
            // Inline SVG can be large and holds nothing for us.
            i = raw_text(b, html, end, b"svg").1;
            continue;
        }
        i = end;
    }
    c.finish()
}

/// Reads attributes from just after the tag name. Returns the index after `>`.
fn read_attrs<'a>(b: &[u8], s: &'a str, mut j: usize, out: &mut Vec<(&'a str, &'a str)>) -> usize {
    let n = b.len();
    loop {
        while j < n && (b[j].is_ascii_whitespace() || b[j] == b'/') {
            j += 1;
        }
        if j >= n {
            return n;
        }
        if b[j] == b'>' {
            return j + 1;
        }
        let ks = j;
        while j < n && !b[j].is_ascii_whitespace() && !matches!(b[j], b'=' | b'>' | b'/') {
            j += 1;
        }
        if j == ks {
            j += 1; // a stray '='
            continue;
        }
        let key = &s[ks..j];
        while j < n && b[j].is_ascii_whitespace() {
            j += 1;
        }
        let mut val = "";
        if j < n && b[j] == b'=' {
            j += 1;
            while j < n && b[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < n && (b[j] == b'"' || b[j] == b'\'') {
                let vs = j + 1;
                match memchr(b[j], &b[vs..]) {
                    Some(e) => {
                        val = &s[vs..vs + e];
                        j = vs + e + 1;
                    }
                    None => return n,
                }
            } else {
                let vs = j;
                while j < n && !b[j].is_ascii_whitespace() && b[j] != b'>' {
                    j += 1;
                }
                val = &s[vs..j];
            }
        }
        if out.len() < MAX_ATTRS {
            out.push((key, val));
        }
    }
}

/// Text up to `</name>`, and the index after that closing tag.
fn raw_text<'a>(b: &[u8], s: &'a str, from: usize, name: &[u8]) -> (&'a str, usize) {
    let mut i = from;
    while let Some(off) = memchr(b'<', &b[i..]) {
        let at = i + off;
        let r = &b[at + 1..];
        if r.first() == Some(&b'/')
            && r.len() > name.len()
            && r[1..=name.len()].eq_ignore_ascii_case(name)
            && !r.get(name.len() + 1).is_some_and(u8::is_ascii_alphanumeric)
        {
            let close = memchr(b'>', &b[at..]).map_or(b.len(), |e| at + e + 1);
            return (&s[from..at], close);
        }
        i = at + 1;
    }
    (&s[from.min(s.len())..], b.len())
}

fn attr<'a>(attrs: &[(&'a str, &'a str)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| *v)
}

fn some(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

fn set(slot: &mut Option<String>, v: &str) {
    if slot.is_none() {
        *slot = some(decode(v));
    }
}

#[derive(Default)]
struct Collector {
    url: String,
    base: Option<Url>,
    base_set: bool,
    og_title: Option<String>,
    tw_title: Option<String>,
    title_tag: Option<String>,
    md_name: Option<String>,
    og_desc: Option<String>,
    tw_desc: Option<String>,
    desc: Option<String>,
    md_desc: Option<String>,
    og_site: Option<String>,
    app_name: Option<String>,
    kind: Option<String>,
    og_locale: Option<String>,
    html_lang: Option<String>,
    canonical: Option<String>,
    og_url: Option<String>,
    images: Vec<Media>,
    videos: Vec<Media>,
    audios: Vec<Media>,
    tw_image: Option<Media>,
    image_src: Option<String>,
    md_image: Option<String>,
    icons: Vec<Icon>,
    theme: Option<String>,
    author: Option<String>,
    article_author: Option<String>,
    published: Option<String>,
    keywords: Vec<String>,
    twitter: Twitter,
    feeds: Vec<Feed>,
    oembed: Option<String>,
    refresh: Option<String>,
}

#[derive(Clone, Copy)]
enum Slot {
    Image,
    Video,
    Audio,
}

impl Collector {
    fn new(url: &str) -> Self {
        Collector {
            url: url.to_owned(),
            base: Url::parse(url).ok(),
            ..Default::default()
        }
    }

    /// An absolute http(s) URL, or nothing.
    fn abs(&self, raw: &str) -> Option<String> {
        let raw = decode(raw);
        if raw.is_empty() {
            return None;
        }
        let u = match &self.base {
            Some(b) => b.join(&raw).ok()?,
            None => Url::parse(&raw).ok()?,
        };
        matches!(u.scheme(), "http" | "https").then(|| u.into())
    }

    fn base(&mut self, attrs: &[(&str, &str)]) {
        if self.base_set {
            return;
        }
        if let Some(h) = attr(attrs, "href").and_then(|h| self.abs(h)) {
            self.base = Url::parse(&h).ok();
            self.base_set = true;
        }
    }

    fn list(&mut self, slot: Slot) -> &mut Vec<Media> {
        match slot {
            Slot::Image => &mut self.images,
            Slot::Video => &mut self.videos,
            Slot::Audio => &mut self.audios,
        }
    }

    fn media(&mut self, slot: Slot, prop: &str, content: &str) {
        match prop {
            "" | ":url" | ":secure_url" => {
                let Some(u) = self.abs(content) else { return };
                let list = self.list(slot);
                let last_open = list.last().is_some_and(|m| m.url.is_empty());
                if prop == ":secure_url" {
                    // Upgrades the current entry to https, or starts one.
                    let full = list.len() >= MAX_IMAGES;
                    match list.last_mut() {
                        Some(m)
                            if m.url.is_empty()
                                || (m.url.starts_with("http:") && u.starts_with("https:")) =>
                        {
                            m.url = u;
                        }
                        Some(m) if m.url == u => {}
                        _ if !full => list.push(Media {
                            url: u,
                            ..Default::default()
                        }),
                        _ => {}
                    }
                } else if last_open {
                    if let Some(m) = list.last_mut() {
                        m.url = u;
                    }
                } else if list.len() < MAX_IMAGES && !list.iter().any(|m| m.url == u) {
                    list.push(Media {
                        url: u,
                        ..Default::default()
                    });
                } else if list.len() < MAX_IMAGES {
                    // A duplicate: later properties should not edit an earlier entry.
                    list.push(Media::default());
                }
            }
            ":width" | ":height" | ":alt" | ":type" => {
                let list = self.list(slot);
                let m = match list.last_mut() {
                    Some(m) => m,
                    None => {
                        list.push(Media::default());
                        list.last_mut().expect("just pushed")
                    }
                };
                let v = decode(content);
                match prop {
                    ":width" => m.width = m.width.or_else(|| v.parse().ok()),
                    ":height" => m.height = m.height.or_else(|| v.parse().ok()),
                    ":alt" => m.alt = m.alt.take().or_else(|| some(v)),
                    _ => m.mime = m.mime.take().or_else(|| some(v)),
                }
            }
            _ => {}
        }
    }

    fn meta(&mut self, attrs: &[(&str, &str)]) {
        let mut key = "";
        let mut microdata = false;
        let mut content = None;
        let mut equiv = None;
        for &(k, v) in attrs {
            if k.eq_ignore_ascii_case("property")
                || (key.is_empty() && k.eq_ignore_ascii_case("name"))
            {
                key = v;
                microdata = false;
            } else if key.is_empty() && k.eq_ignore_ascii_case("itemprop") {
                key = v;
                microdata = true;
            } else if k.eq_ignore_ascii_case("content") {
                content = Some(v);
            } else if k.eq_ignore_ascii_case("http-equiv") {
                equiv = Some(v);
            }
        }
        let Some(content) = content else { return };
        if let Some(e) = equiv {
            if e.eq_ignore_ascii_case("refresh") && self.refresh.is_none() {
                let target = content
                    .split_once(';')
                    .map(|(_, r)| r.trim())
                    .and_then(|r| {
                        r.get(..4)
                            .filter(|p| p.eq_ignore_ascii_case("url="))
                            .map(|_| &r[4..])
                    })
                    .map(|r| r.trim().trim_matches(['"', '\'']));
                self.refresh = target.and_then(|t| self.abs(t));
            }
            return;
        }
        if key.is_empty() || content.trim().is_empty() {
            return;
        }
        let key = key.trim().to_ascii_lowercase();
        if microdata {
            match key.as_str() {
                "name" => set(&mut self.md_name, content),
                "description" => set(&mut self.md_desc, content),
                "image" | "thumbnailurl" if self.md_image.is_none() => {
                    self.md_image = self.abs(content)
                }
                "datepublished" | "uploaddate" => set(&mut self.published, content),
                _ => {}
            }
            return;
        }
        if let Some(rest) = key.strip_prefix("og:image") {
            return self.media(Slot::Image, rest, content);
        }
        if let Some(rest) = key.strip_prefix("og:video") {
            return self.media(Slot::Video, rest, content);
        }
        if let Some(rest) = key.strip_prefix("og:audio") {
            return self.media(Slot::Audio, rest, content);
        }
        match key.as_str() {
            "og:title" => set(&mut self.og_title, content),
            "og:description" => set(&mut self.og_desc, content),
            "og:site_name" => set(&mut self.og_site, content),
            "og:type" => set(&mut self.kind, content),
            "og:locale" => set(&mut self.og_locale, content),
            "og:url" if self.og_url.is_none() => self.og_url = self.abs(content),
            "twitter:title" => set(&mut self.tw_title, content),
            "twitter:description" => set(&mut self.tw_desc, content),
            "twitter:image" | "twitter:image:src" if self.tw_image.is_none() => {
                self.tw_image = self.abs(content).map(|url| Media {
                    url,
                    ..Default::default()
                });
            }
            "twitter:image:alt" => {
                if let Some(m) = &mut self.tw_image {
                    m.alt = m.alt.take().or_else(|| some(decode(content)));
                }
            }
            "twitter:card" => set(&mut self.twitter.card, content),
            "twitter:site" => set(&mut self.twitter.site, content),
            "twitter:creator" => set(&mut self.twitter.creator, content),
            "description" => set(&mut self.desc, content),
            "application-name" => set(&mut self.app_name, content),
            "theme-color" => set(&mut self.theme, content),
            "author" => set(&mut self.author, content),
            "article:author" => set(&mut self.article_author, content),
            "article:published_time" | "date" | "pubdate" => set(&mut self.published, content),
            "keywords" if self.keywords.is_empty() => {
                self.keywords = decode(content)
                    .split([',', '，'])
                    .map(str::trim)
                    .filter(|k| !k.is_empty())
                    .take(MAX_KEYWORDS)
                    .map(str::to_owned)
                    .collect();
            }
            _ => {}
        }
    }

    fn link(&mut self, attrs: &[(&str, &str)]) {
        let (Some(rel), Some(href)) = (attr(attrs, "rel"), attr(attrs, "href")) else {
            return;
        };
        let rel = rel.to_ascii_lowercase();
        let rel = rel.split_ascii_whitespace().collect::<Vec<_>>();
        let has = |r: &str| rel.contains(&r);
        let mime = attr(attrs, "type").map(|t| t.trim().to_ascii_lowercase());
        if has("icon")
            || has("apple-touch-icon")
            || has("apple-touch-icon-precomposed")
            || has("mask-icon")
        {
            if self.icons.len() < MAX_ICONS
                && let Some(url) = self.abs(href)
            {
                let size = attr(attrs, "sizes").and_then(icon_size);
                self.icons.push(Icon {
                    url,
                    rel: rel.join(" "),
                    size,
                    mime: mime.clone(),
                });
            }
        } else if has("canonical") {
            if self.canonical.is_none() {
                self.canonical = self.abs(href);
            }
        } else if has("alternate") {
            match mime.as_deref() {
                Some(
                    m @ ("application/rss+xml" | "application/atom+xml" | "application/feed+json"),
                ) if self.feeds.len() < MAX_FEEDS => {
                    if let Some(url) = self.abs(href) {
                        let title = attr(attrs, "title").and_then(|t| some(decode(t)));
                        self.feeds.push(Feed {
                            url,
                            title,
                            mime: m.to_owned(),
                        });
                    }
                }
                Some("application/json+oembed" | "text/json+oembed") if self.oembed.is_none() => {
                    self.oembed = self.abs(href);
                }
                _ => {}
            }
        } else if has("image_src") && self.image_src.is_none() {
            self.image_src = self.abs(href);
        }
    }

    fn finish(self) -> Page {
        let mut images: Vec<Media> = self
            .images
            .into_iter()
            .filter(|m| !m.url.is_empty())
            .collect();
        if images.is_empty() {
            let fallback = self
                .tw_image
                .or_else(|| {
                    self.image_src.map(|url| Media {
                        url,
                        ..Default::default()
                    })
                })
                .or_else(|| {
                    self.md_image.map(|url| Media {
                        url,
                        ..Default::default()
                    })
                });
            images.extend(fallback);
        }
        let favicon = self
            .icons
            .iter()
            .filter(|i| i.rel != "mask-icon")
            .enumerate()
            .max_by_key(|(n, i)| (i.size.unwrap_or(0), std::cmp::Reverse(*n)))
            .map(|(_, i)| i.url.clone())
            .or_else(|| {
                let b = self
                    .base
                    .as_ref()
                    .filter(|b| matches!(b.scheme(), "http" | "https"))?;
                b.join("/favicon.ico").ok().map(String::from)
            });
        Page {
            url: self.url,
            title: self
                .og_title
                .or(self.tw_title)
                .or(self.title_tag)
                .or(self.md_name),
            description: self.og_desc.or(self.tw_desc).or(self.desc).or(self.md_desc),
            site_name: self.og_site.or(self.app_name),
            kind: self.kind,
            locale: self.og_locale.or(self.html_lang),
            canonical: self.canonical.or(self.og_url),
            images,
            videos: self
                .videos
                .into_iter()
                .filter(|m| !m.url.is_empty())
                .collect(),
            audios: self
                .audios
                .into_iter()
                .filter(|m| !m.url.is_empty())
                .collect(),
            icons: self.icons,
            favicon,
            theme_color: self.theme,
            author: self.author.or(self.article_author),
            published: self.published,
            keywords: self.keywords,
            twitter: self.twitter,
            feeds: self.feeds,
            oembed: self.oembed,
            content_type: None,
            refresh: self.refresh,
        }
    }
}

/// Largest side from a `sizes` attribute: `"16x16 32x32"` gives 32, `"any"` gives 1024.
fn icon_size(sizes: &str) -> Option<u32> {
    sizes
        .split_ascii_whitespace()
        .filter_map(|s| {
            if s.eq_ignore_ascii_case("any") {
                return Some(1024);
            }
            let (w, h) = s.split_once(['x', 'X'])?;
            Some(w.parse::<u32>().ok()?.max(h.parse().ok()?))
        })
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_case_insensitively() {
        assert_eq!(find_ci(b"abc</HEAD>", b"</head"), Some(3));
        assert_eq!(find_ci(b"abc", b"abcd"), None);
        assert_eq!(head_end(b"<html><meta x><BODY>"), Some(14));
        assert_eq!(head_end(b"<html><meta x></Head>"), Some(14));
        assert_eq!(head_end(b"<html><meta x>"), None);
    }

    #[test]
    fn icon_sizes() {
        assert_eq!(icon_size("16x16 192X192"), Some(192));
        assert_eq!(icon_size("any"), Some(1024));
        assert_eq!(icon_size("big"), None);
    }

    #[test]
    fn header_charset_forms() {
        assert_eq!(header_charset("text/html; charset=GBK"), Some(&b"GBK"[..]));
        assert_eq!(
            header_charset("text/html;charset=\"utf-8\""),
            Some(&b"utf-8"[..])
        );
        assert_eq!(header_charset("text/html"), None);
    }
}
