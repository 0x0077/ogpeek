//! JSON-LD (`<script type="application/ld+json">`): finds the main entity and summarizes it.

use serde_json::{Map, Value};

use crate::entities::clean_text;
use crate::page::Schema;

/// How much a `@type` tells about what the page is. Zero means "not the page's subject".
fn weight(kind: &str) -> u8 {
    match kind {
        "Article"
        | "NewsArticle"
        | "BlogPosting"
        | "TechArticle"
        | "ScholarlyArticle"
        | "Report"
        | "SocialMediaPosting"
        | "DiscussionForumPosting"
        | "Product"
        | "ProductGroup"
        | "VideoObject"
        | "Recipe"
        | "Event"
        | "Book"
        | "Movie"
        | "TVSeries"
        | "TVEpisode"
        | "MusicRecording"
        | "MusicAlbum"
        | "MusicPlaylist"
        | "PodcastSeries"
        | "PodcastEpisode"
        | "SoftwareApplication"
        | "MobileApplication"
        | "WebApplication"
        | "VideoGame"
        | "Course"
        | "JobPosting"
        | "Dataset"
        | "CreativeWork"
        | "Review" => 10,
        "Person" | "MusicGroup" | "LocalBusiness" | "Restaurant" | "Store" | "Place" | "Hotel" => 8,
        "ProfilePage" | "QAPage" | "FAQPage" | "ItemPage" => 6,
        "WebPage" | "AboutPage" | "CollectionPage" | "ContactPage" => 4,
        "Organization" | "Corporation" | "NewsMediaOrganization" | "EducationalOrganization" => 3,
        "WebSite" => 2,
        _ => 0,
    }
}

/// The first `@type` of a node.
fn kind_of(node: &Map<String, Value>) -> Option<&str> {
    match node.get("@type")? {
        Value::String(s) => Some(s.as_str()),
        Value::Array(a) => a.iter().find_map(Value::as_str),
        _ => None,
    }
}

/// Every object worth considering: top-level objects, arrays, `@graph` members, and the
/// `mainEntity` of a page (which outranks the page itself).
fn collect<'a>(v: &'a Value, out: &mut Vec<(&'a Map<String, Value>, u8)>, depth: u8) {
    if depth > 4 {
        return;
    }
    match v {
        Value::Array(a) => a.iter().take(64).for_each(|x| collect(x, out, depth + 1)),
        Value::Object(o) => {
            if let Some(g) = o.get("@graph") {
                collect(g, out, depth + 1);
            }
            if let Some(k) = kind_of(o) {
                out.push((o, weight(k)));
            }
            if let Some(Value::Object(m)) =
                o.get("mainEntity").or_else(|| o.get("mainEntityOfPage"))
                && let Some(k) = kind_of(m)
            {
                out.push((m, weight(k).saturating_add(1)));
            }
        }
        _ => {}
    }
}

fn text(v: &Value) -> Option<String> {
    let s = match v {
        Value::String(s) => clean_text(s),
        Value::Number(n) => n.to_string(),
        Value::Array(a) => return a.iter().find_map(text),
        Value::Object(o) => return o.get("name").or_else(|| o.get("@value")).and_then(text),
        _ => return None,
    };
    (!s.is_empty()).then_some(s)
}

fn url_of(v: &Value) -> Option<&str> {
    match v {
        Value::String(s) => Some(s.as_str()),
        Value::Array(a) => a.iter().find_map(url_of),
        Value::Object(o) => o
            .get("url")
            .or_else(|| o.get("contentUrl"))
            .and_then(url_of),
        _ => None,
    }
}

fn first<'a>(node: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .find_map(|k| node.get(*k).filter(|v| !v.is_null()))
}

/// Parses the JSON-LD blocks and summarizes the most specific entity. `abs` resolves
/// image URLs against the page. Returns the summary and how specific it is.
pub(crate) fn summarize(
    blocks: &[String],
    page_urls: &[&str],
    abs: impl Fn(&str) -> Option<String>,
) -> Option<(Schema, u8)> {
    let values: Vec<Value> = blocks.iter().filter_map(|b| parse_block(b)).collect();
    let mut nodes = Vec::new();
    for v in &values {
        collect(v, &mut nodes, 0);
    }
    let pages: Vec<String> = page_urls.iter().filter_map(|u| same_page_key(u)).collect();
    let mut counts = std::collections::HashMap::new();
    for (n, _) in &nodes {
        *counts.entry(kind_of(n).unwrap_or_default()).or_insert(0u32) += 1;
    }
    // A node whose url is this page is its subject; a type repeated many times is a list
    // (a profile's posts, a shop's products), not the subject.
    let scored = nodes.into_iter().filter(|(_, w)| *w > 0).map(|(n, w)| {
        let own = ["url", "@id", "mainEntityOfPage"]
            .iter()
            .filter_map(|k| n.get(*k).and_then(url_of))
            .filter_map(same_page_key)
            .any(|k| pages.contains(&k));
        let repeated = counts
            .get(kind_of(n).unwrap_or_default())
            .copied()
            .unwrap_or(0)
            > 1;
        let w = w
            .saturating_add(if own { 5 } else { 0 })
            .saturating_sub(if repeated { 4 } else { 0 });
        (n, w)
    });
    // Highest weight wins; the first one on the page wins a tie.
    let (node, w) = scored
        .enumerate()
        .max_by_key(|(i, (_, w))| (*w, std::cmp::Reverse(*i)))
        .map(|(_, n)| n)?;
    let offer = node.get("offers").map(|o| match o {
        Value::Array(a) => a.first().unwrap_or(o),
        _ => o,
    });
    let schema = Schema {
        kind: kind_of(node).unwrap_or_default().to_owned(),
        name: first(node, &["headline", "name", "alternateName"]).and_then(text),
        description: first(node, &["description", "abstract"]).and_then(text),
        image: first(node, &["image", "thumbnailUrl", "logo", "photo"])
            .and_then(url_of)
            .and_then(&abs),
        author: first(
            node,
            &["author", "creator", "byArtist", "director", "brand"],
        )
        .and_then(text),
        published: first(
            node,
            &["datePublished", "uploadDate", "startDate", "dateCreated"],
        )
        .and_then(text),
        price: offer
            .and_then(|o| o.get("price").or_else(|| o.get("lowPrice")))
            .and_then(text),
        currency: offer.and_then(|o| o.get("priceCurrency")).and_then(text),
    };
    Some((schema, w))
}

/// Host and path without `www.`, scheme, query or trailing slash, for comparing addresses.
fn same_page_key(u: &str) -> Option<String> {
    let u = url::Url::parse(u).ok()?;
    let host = u.host_str()?.trim_start_matches("www.");
    Some(format!("{host}{}", u.path().trim_end_matches('/')).to_lowercase())
}

/// Sites wrap JSON-LD in comments or CDATA, and some leave raw newlines inside strings.
fn parse_block(raw: &str) -> Option<Value> {
    let mut s = raw.trim();
    for (open, close) in [
        ("<!--", "-->"),
        ("//<![CDATA[", "//]]>"),
        ("<![CDATA[", "]]>"),
    ] {
        if let Some(inner) = s.strip_prefix(open) {
            s = inner.strip_suffix(close).unwrap_or(inner).trim();
        }
    }
    serde_json::from_str(s).ok().or_else(|| {
        let cleaned: String = s
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        serde_json::from_str(&cleaned).ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(s: &str) -> Option<String> {
        Some(if s.starts_with('/') {
            format!("https://shop.example{s}")
        } else {
            s.to_owned()
        })
    }

    #[test]
    fn picks_the_product_over_the_site() {
        let blocks = vec![
            r#"{"@context":"https://schema.org","@type":"WebSite","name":"Shop"}"#.to_owned(),
            r#"<!-- {"@context":"https://schema.org","@graph":[
                {"@type":"BreadcrumbList","itemListElement":[]},
                {"@type":["Product"],"name":"Linen shirt &amp; tie","image":[{"@type":"ImageObject","url":"/shirt.jpg"}],
                 "brand":{"@type":"Brand","name":"Atelier"},
                 "offers":[{"@type":"Offer","price":49.5,"priceCurrency":"EUR"}]}]} -->"#
                .to_owned(),
        ];
        let (s, w) = summarize(&blocks, &[], abs).unwrap();
        assert_eq!(w, 10);
        assert_eq!(s.kind, "Product");
        assert_eq!(s.name.as_deref(), Some("Linen shirt & tie"));
        assert_eq!(s.image.as_deref(), Some("https://shop.example/shirt.jpg"));
        assert_eq!(s.author.as_deref(), Some("Atelier"));
        assert_eq!(
            (s.price.as_deref(), s.currency.as_deref()),
            (Some("49.5"), Some("EUR"))
        );
    }

    #[test]
    fn profile_page_main_entity() {
        let blocks = vec![
            r#"{"@type":"ProfilePage","mainEntity":{"@type":"Person","name":"Ada","description":"Engineer","image":"https://a.example/ada.png"}}"#
                .to_owned(),
        ];
        let (s, _) = summarize(&blocks, &[], abs).unwrap();
        assert_eq!(s.kind, "Person");
        assert_eq!(s.name.as_deref(), Some("Ada"));
        assert_eq!(s.image.as_deref(), Some("https://a.example/ada.png"));
    }

    #[test]
    fn the_page_subject_beats_a_list_of_posts() {
        let blocks = vec![
            r#"{"@graph":[
                {"@type":"Article","headline":"Post one","url":"https://social.example/p/1"},
                {"@type":"Article","headline":"Post two","url":"https://social.example/p/2"},
                {"@type":"Person","name":"Ada Lovelace","url":"https://www.social.example/in/ada/"}]}"#
                .to_owned(),
        ];
        let (s, w) = summarize(&blocks, &["https://social.example/in/ada"], abs).unwrap();
        assert_eq!(
            (s.kind.as_str(), s.name.as_deref()),
            ("Person", Some("Ada Lovelace"))
        );
        assert!(w >= 8);
        // Without knowing the page, the repeated articles still lose.
        let (s, _) = summarize(&blocks, &[], abs).unwrap();
        assert_eq!(s.kind, "Person");
    }

    #[test]
    fn tolerates_broken_blocks() {
        let blocks = vec![
            "{not json".to_owned(),
            "{\"@type\":\"Article\",\"headline\":\"Two\nlines\"}".to_owned(),
        ];
        let (s, _) = summarize(&blocks, &[], abs).unwrap();
        assert_eq!(s.name.as_deref(), Some("Two lines"));
        assert!(summarize(&["[]".to_owned()], &[], abs).is_none());
    }
}
