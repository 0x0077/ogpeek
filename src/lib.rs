//! Fast link previews.
//!
//! ogpeek reads what a web page says about itself (Open Graph, Twitter cards, `<title>`,
//! description, icons, feeds, oEmbed) as quickly as the remote server allows:
//!
//! - **Stops at `</head>`.** Everything a preview needs is in the head, so the rest of the
//!   page is never downloaded, which matters most for large pages and slow links.
//! - **One pass, no DOM.** The parser walks the bytes once with `memchr`, reads only the
//!   tags it needs and skips scripts, styles and comments without looking inside them.
//! - **Shares everything.** One connection pool (HTTP/2, keep-alive, compressed transfer),
//!   a DNS cache, a preview cache, and concurrent requests for the same URL become one.
//! - **Safe on a server.** Hosts that resolve to private, loopback, link-local or cloud
//!   metadata addresses are refused, at the resolver, for every redirect hop. Sizes,
//!   redirects and time are capped.
//! - **Asks the source.** For YouTube, TikTok, Vimeo, Spotify, SoundCloud, X posts and
//!   other providers, the official oEmbed endpoint is asked alongside the page. JSON-LD in
//!   the head fills what Open Graph leaves out.
//! - **Any language.** Pages in GBK, Shift_JIS, EUC-KR, Windows-1251 and the other WHATWG
//!   encodings are decoded correctly.
//!
//! ```no_run
//! # #[cfg(feature = "fetch")]
//! # async fn run() -> Result<(), ogpeek::Error> {
//! let peeker = ogpeek::Peeker::new();
//! let page = peeker.peek("https://github.com/rust-lang/rust").await?;
//! println!("{}", page.title.as_deref().unwrap_or(""));
//! if let Some(image) = page.image() {
//!     println!("{}", image.url);
//! }
//! # Ok(()) }
//! ```
//!
//! Without the default `fetch` feature, ogpeek is only the parser: [`parse`] and
//! [`parse_bytes`] take HTML you already have.

mod entities;
mod jsonld;
mod oembed;
mod page;
mod parse;

#[cfg(feature = "fetch")]
mod net;
#[cfg(feature = "fetch")]
mod peeker;

pub use entities::{clean_text, decode};
pub use oembed::{oembed_endpoint, oembed_hosts, parse_oembed};
pub use page::{Embed, Feed, Icon, Media, Page, Schema, Twitter};
pub use parse::{decode_html, parse, parse_bytes, parse_document};

#[cfg(feature = "fetch")]
pub use net::{Error, Resolver, forbidden_ip};
#[cfg(feature = "fetch")]
pub use peeker::{Builder, Fetched, Peeker};

/// Reads the preview for one URL with a shared default [`Peeker`].
///
/// Convenient for scripts. A long-running program should keep its own `Peeker`.
#[cfg(feature = "fetch")]
pub async fn peek(url: &str) -> Result<std::sync::Arc<Page>, Error> {
    static SHARED: std::sync::LazyLock<Peeker> = std::sync::LazyLock::new(Peeker::new);
    SHARED.peek(url).await
}
