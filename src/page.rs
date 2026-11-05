//! What a page says about itself.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Everything ogpeek read from a page's `<head>`.
///
/// Fields are already resolved: URLs are absolute (http or https only), text has its
/// character references decoded and whitespace collapsed, and the common fallbacks are
/// applied (`og:title`, then `twitter:title`, then `<title>`, and so on).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase", default))]
pub struct Page {
    /// The address the page was read from, after redirects.
    pub url: String,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub title: Option<String>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub description: Option<String>,
    /// `og:site_name`, then `application-name`.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub site_name: Option<String>,
    /// `og:type`, such as `website`, `article`, `video.other`. `image` when the URL was an image.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub kind: Option<String>,
    /// `og:locale`, then `<html lang>`.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub locale: Option<String>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub canonical: Option<String>,
    /// `og:image` entries in page order, then `twitter:image`, then `<link rel=image_src>`.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty"))]
    pub images: Vec<Media>,
    /// `og:video` entries.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty"))]
    pub videos: Vec<Media>,
    /// `og:audio` entries.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty"))]
    pub audios: Vec<Media>,
    /// Every `<link rel=icon>`, `apple-touch-icon` and `mask-icon`.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty"))]
    pub icons: Vec<Icon>,
    /// The best icon to show: the largest declared one, or `/favicon.ico` when the page names none.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub favicon: Option<String>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub theme_color: Option<String>,
    /// `author`, then `article:author`.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub author: Option<String>,
    /// `article:published_time`, as written by the page.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub published: Option<String>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty"))]
    pub keywords: Vec<String>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Twitter::is_empty"))]
    pub twitter: Twitter,
    /// RSS, Atom and JSON Feed links (`<link rel=alternate>`).
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty"))]
    pub feeds: Vec<Feed>,
    /// The page's JSON oEmbed endpoint, when it advertises one.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub oembed: Option<String>,
    /// The `Content-Type` the server sent, when the page was fetched.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub content_type: Option<String>,
    /// Target of `<meta http-equiv=refresh>`, when present.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub refresh: Option<String>,
    /// The main thing the page's JSON-LD (schema.org) describes, when it has any.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub schema: Option<Schema>,
    /// What the site's oEmbed endpoint said, when one was asked.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub embed: Option<Embed>,
}

impl Page {
    /// The first image, which is the one sites mean as the preview image.
    pub fn image(&self) -> Option<&Media> {
        self.images.first()
    }

    /// True when the page gave nothing a preview could show.
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.description.is_none() && self.images.is_empty()
    }
}

/// An image, video or audio from Open Graph.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase", default))]
pub struct Media {
    pub url: String,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub width: Option<u32>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub height: Option<u32>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub alt: Option<String>,
    /// MIME type, such as `image/png`.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub mime: Option<String>,
}

/// A declared icon.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase", default))]
pub struct Icon {
    pub url: String,
    /// The `rel` value, lowercased: `icon`, `shortcut icon`, `apple-touch-icon`, `mask-icon`.
    pub rel: String,
    /// Largest side from `sizes` (`any` counts as 1024).
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub size: Option<u32>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub mime: Option<String>,
}

/// A feed the page links to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase", default))]
pub struct Feed {
    pub url: String,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub title: Option<String>,
    /// `application/rss+xml`, `application/atom+xml` or `application/feed+json`.
    pub mime: String,
}

/// Twitter card fields that have no Open Graph equivalent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase", default))]
pub struct Twitter {
    /// `summary`, `summary_large_image`, `player`, `app`.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub card: Option<String>,
    /// The site's handle, such as `@nasa`.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub site: Option<String>,
    /// The author's handle.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub creator: Option<String>,
}

impl Twitter {
    pub fn is_empty(&self) -> bool {
        self.card.is_none() && self.site.is_none() && self.creator.is_none()
    }
}

/// A summary of the main entity in a page's JSON-LD: the article, product, video, person
/// or organization the page is about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase", default))]
pub struct Schema {
    /// The schema.org `@type`, such as `Article`, `Product`, `VideoObject`, `Person`.
    pub kind: String,
    /// `headline` or `name`.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub name: Option<String>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub description: Option<String>,
    /// `image`, `thumbnailUrl` or `logo`, absolute.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub image: Option<String>,
    /// The author's or creator's name.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub author: Option<String>,
    /// `datePublished`, `uploadDate` or `startDate`, as written.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub published: Option<String>,
    /// A product's price from `offers`, as written (`"19.99"`).
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub price: Option<String>,
    /// ISO 4217 code from `offers.priceCurrency`.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub currency: Option<String>,
}

/// An oEmbed response.
///
/// `html` is the provider's own embed code (usually an `<iframe>` or a `<blockquote>` with a
/// script). It comes from the provider, not from ogpeek: render it only inside a sandboxed
/// iframe or after your own sanitizing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase", default))]
pub struct Embed {
    /// `video`, `photo`, `rich` or `link`.
    pub kind: String,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub title: Option<String>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub author_name: Option<String>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub author_url: Option<String>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub provider_name: Option<String>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub provider_url: Option<String>,
    /// The thumbnail, or for a `photo` the photo itself.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub thumbnail: Option<Media>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub html: Option<String>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub width: Option<u32>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub height: Option<u32>,
}
