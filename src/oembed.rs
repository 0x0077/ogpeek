//! oEmbed: a built-in list of providers with public endpoints, and response parsing.
//!
//! Only providers whose endpoints need no API key and answer a server with JSON are
//! listed, each checked against real URLs. Instagram, Facebook and Threads need an app
//! token; Reddit, Kickstarter, CodePen and others answer servers with a block page.
//! Pages from any other site that advertise an endpoint with `<link rel=alternate>` are
//! still handled through discovery.

use serde_json::Value;
use url::Url;

use crate::entities::clean_text;
use crate::page::{Embed, Media};

struct Provider {
    hosts: &'static [&'static str],
    /// Which paths on those hosts the endpoint accepts.
    matches: fn(&Url) -> bool,
    /// The endpoint; the page URL is appended, percent-encoded.
    endpoint: &'static str,
}

fn not_root(u: &Url) -> bool {
    u.path()
        .trim_matches('/')
        .contains(|c: char| c.is_alphanumeric())
}

fn path_has(u: &Url, parts: &[&str]) -> bool {
    parts.iter().any(|p| u.path().contains(p))
}

const PROVIDERS: &[Provider] = &[
    Provider {
        hosts: &["youtube.com"],
        matches: |u| path_has(u, &["/watch", "/shorts/", "/live/", "/playlist", "/embed/"]),
        endpoint: "https://www.youtube.com/oembed?format=json&url=",
    },
    Provider {
        hosts: &["youtu.be"],
        matches: not_root,
        endpoint: "https://www.youtube.com/oembed?format=json&url=",
    },
    Provider {
        hosts: &["vimeo.com"],
        matches: not_root,
        endpoint: "https://vimeo.com/api/oembed.json?url=",
    },
    Provider {
        hosts: &["tiktok.com"],
        matches: |u| u.path().starts_with("/@"),
        endpoint: "https://www.tiktok.com/oembed?url=",
    },
    Provider {
        hosts: &["open.spotify.com"],
        matches: not_root,
        endpoint: "https://open.spotify.com/oembed?url=",
    },
    Provider {
        hosts: &["soundcloud.com"],
        matches: not_root,
        endpoint: "https://soundcloud.com/oembed?format=json&url=",
    },
    Provider {
        hosts: &["mixcloud.com"],
        matches: not_root,
        endpoint: "https://app.mixcloud.com/oembed/?format=json&url=",
    },
    Provider {
        hosts: &["x.com", "twitter.com"],
        matches: |u| path_has(u, &["/status/"]),
        endpoint: "https://publish.twitter.com/oembed?omit_script=1&url=",
    },
    Provider {
        hosts: &["bsky.app"],
        matches: |u| path_has(u, &["/post/"]),
        endpoint: "https://embed.bsky.app/oembed?format=json&url=",
    },
    Provider {
        hosts: &["flickr.com", "flic.kr"],
        matches: |u| u.host_str() == Some("flic.kr") || path_has(u, &["/photos/"]),
        endpoint: "https://www.flickr.com/services/oembed/?format=json&url=",
    },
    Provider {
        hosts: &["dailymotion.com", "dai.ly"],
        matches: |u| u.host_str() == Some("dai.ly") || path_has(u, &["/video/"]),
        endpoint: "https://www.dailymotion.com/services/oembed?format=json&url=",
    },
    Provider {
        hosts: &["loom.com"],
        matches: |u| path_has(u, &["/share/"]),
        endpoint: "https://www.loom.com/v1/oembed?format=json&url=",
    },
    Provider {
        hosts: &["ted.com"],
        matches: |u| path_has(u, &["/talks/"]),
        endpoint: "https://www.ted.com/services/v1/oembed.json?url=",
    },
    Provider {
        hosts: &["giphy.com"],
        matches: |u| path_has(u, &["/gifs/", "/clips/"]),
        endpoint: "https://giphy.com/services/oembed?url=",
    },
];

/// The oEmbed endpoint for `url` when it belongs to a known provider, with the URL filled in.
///
/// ```
/// let e = ogpeek::oembed_endpoint("https://youtu.be/dQw4w9WgXcQ").unwrap();
/// assert_eq!(e, "https://www.youtube.com/oembed?format=json&url=https%3A%2F%2Fyoutu.be%2FdQw4w9WgXcQ");
/// assert!(ogpeek::oembed_endpoint("https://example.com/").is_none());
/// ```
pub fn oembed_endpoint(url: &str) -> Option<String> {
    let u = Url::parse(url).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    let host = u
        .host_str()?
        .trim_start_matches("www.")
        .trim_start_matches("m.");
    let p = PROVIDERS.iter().find(|p| {
        p.hosts
            .iter()
            .any(|h| host == *h || host.ends_with(&format!(".{h}")))
            && (p.matches)(&u)
    })?;
    let encoded: String = url::form_urlencoded::byte_serialize(u.as_str().as_bytes()).collect();
    Some(format!("{}{encoded}", p.endpoint))
}

/// The hosts with a built-in oEmbed provider.
pub fn oembed_hosts() -> impl Iterator<Item = &'static str> {
    PROVIDERS.iter().flat_map(|p| p.hosts.iter().copied())
}

fn s(v: &Value, k: &str) -> Option<String> {
    let t = match v.get(k)? {
        Value::String(s) => clean_text(s),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    (!t.is_empty()).then_some(t)
}

fn n(v: &Value, k: &str) -> Option<u32> {
    match v.get(k)? {
        Value::Number(x) => x.as_f64().map(|f| f as u32),
        Value::String(x) => x.trim().parse().ok(),
        _ => None,
    }
}

fn http(u: Option<String>) -> Option<String> {
    let u = Url::parse(&u?).ok()?;
    matches!(u.scheme(), "http" | "https").then(|| u.into())
}

/// Reads an oEmbed JSON response. Returns `None` when it is not one.
pub fn parse_oembed(json: &[u8]) -> Option<Embed> {
    let v: Value = serde_json::from_slice(json).ok()?;
    let kind = s(&v, "type")?;
    let thumbnail = http(s(&v, "thumbnail_url"))
        .map(|url| Media {
            url,
            width: n(&v, "thumbnail_width"),
            height: n(&v, "thumbnail_height"),
            ..Default::default()
        })
        .or_else(|| {
            (kind == "photo")
                .then(|| http(s(&v, "url")))
                .flatten()
                .map(|url| Media {
                    url,
                    width: n(&v, "width"),
                    height: n(&v, "height"),
                    ..Default::default()
                })
        });
    Some(Embed {
        title: s(&v, "title"),
        author_name: s(&v, "author_name"),
        author_url: http(s(&v, "author_url")),
        provider_name: s(&v, "provider_name"),
        provider_url: http(s(&v, "provider_url")),
        thumbnail,
        html: v
            .get("html")
            .and_then(Value::as_str)
            .filter(|h| !h.trim().is_empty())
            .map(str::to_owned),
        width: n(&v, "width"),
        height: n(&v, "height"),
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_providers() {
        assert!(oembed_endpoint("https://www.youtube.com/watch?v=abc").is_some());
        assert!(oembed_endpoint("https://m.youtube.com/watch?v=abc").is_some());
        assert!(oembed_endpoint("https://www.youtube.com/@NASA").is_none());
        assert!(
            oembed_endpoint("https://www.tiktok.com/@nasa")
                .unwrap()
                .starts_with("https://www.tiktok.com/oembed?url=")
        );
        assert!(oembed_endpoint("https://x.com/NASA/status/123").is_some());
        assert!(oembed_endpoint("https://x.com/NASA").is_none());
        assert!(oembed_endpoint("https://vimeo.com/").is_none());
        assert!(oembed_endpoint("https://evilyoutube.com/watch").is_none());
        assert!(oembed_hosts().any(|h| h == "soundcloud.com"));
    }

    #[test]
    fn parses_responses() {
        let e = parse_oembed(
            br#"{"type":"video","version":"1.0","title":"Launch &amp; landing","author_name":"NASA",
            "author_url":"https://www.youtube.com/@NASA","provider_name":"YouTube",
            "thumbnail_url":"https://i.ytimg.com/vi/x/hqdefault.jpg","thumbnail_width":480,"thumbnail_height":"360",
            "html":"<iframe src=\"https://www.youtube.com/embed/x\"></iframe>","width":200,"height":113}"#,
        )
        .unwrap();
        assert_eq!(e.kind, "video");
        assert_eq!(e.title.as_deref(), Some("Launch & landing"));
        assert_eq!(
            e.thumbnail.as_ref().map(|t| (t.width, t.height)),
            Some((Some(480), Some(360)))
        );
        assert!(e.html.unwrap().starts_with("<iframe"));
        let p = parse_oembed(br#"{"type":"photo","url":"https://live.staticflickr.com/1.jpg","width":"1024","height":768}"#)
            .unwrap();
        assert_eq!(
            p.thumbnail.unwrap().url,
            "https://live.staticflickr.com/1.jpg"
        );
        assert!(parse_oembed(b"<html>").is_none());
        assert!(parse_oembed(br#"{"title":"no type"}"#).is_none());
    }
}
