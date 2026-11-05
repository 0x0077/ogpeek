# ogpeek

Fast link previews for Rust. Give it a URL; get back the title, description, image, icon,
site name, feeds, the page's JSON-LD subject and the provider's oEmbed answer, usually
before the page has finished downloading.

```rust
let peeker = ogpeek::Peeker::new();
let page = peeker.peek("https://github.com/rust-lang/rust").await?;

println!("{:?}", page.title);
println!("{:?}", page.image().map(|i| &i.url));
println!("{:?}", page.favicon);
```

## Why it is fast

A preview lives in the page's `<head>`. Most link preview libraries download the whole page,
build a DOM, then run selectors over it. ogpeek does less:

- **It stops reading as soon as it has the preview.** Once the title, description, image
  and icon have all been seen, or the head ends, the connection is dropped. A page whose
  tags sit in the first 18 KB of a 788 KB head is answered from those 18 KB.
- **One pass over the bytes, no DOM.** The scanner jumps between `<` with `memchr`, reads
  only `<meta>`, `<link>`, `<title>`, `<base>` and `<html>`, and steps over scripts,
  styles and comments without looking inside them.
- **Nothing is fetched twice.** One connection pool (HTTP/2, keep-alive, gzip, brotli and
  zstd), a DNS cache, and a preview cache. Twenty concurrent requests for the same URL make
  one network request.

Parsing, measured with `cargo bench` on an Apple M4 Max:

| Input | ogpeek | scraper (html5ever + selectors) |
|---|---|---|
| A realistic 38 KB `<head>` (50 tags, inline scripts and styles) | 9 µs | 93 µs |
| A 300 KB page | 9 µs (stops at the head) | 4.1 ms |

On the network, the time is mostly the remote server's; ogpeek's job is to add nothing to
it and to avoid paying it twice. Measured from a server in Los Angeles:

| Request | Time |
|---|---|
| First request to a site (DNS, TCP, TLS, first byte) | what the site takes; same as `curl` |
| Another page on a site already visited (pooled connection) | 73 to 110 ms for Hacker News |
| The same URL again within the cache time | 0.004 ms |

The first request in a new process also loads the system's TLS root certificates once.

## Safe to run on a server

A link preview service fetches URLs that strangers type. ogpeek refuses to connect to:

- loopback, private, link-local and carrier-grade NAT ranges, including cloud metadata
  at `169.254.169.254`, and IPv6 forms that hide an IPv4 address (mapped, NAT64, 6to4);
- hosts whose DNS answer contains any such address, checked in the resolver so that the
  checked address is the connected address (no DNS rebinding);
- non-default ports, user names and passwords in URLs, and schemes other than http(s).

Every redirect hop is checked again. Time, bytes and redirects are all capped.

## What you get

`Page` has the common fallbacks applied, and every URL resolved to absolute http(s):

| Field | From |
|---|---|
| `title` | `og:title`, `twitter:title`, a specific JSON-LD entity's name, `<title>` |
| `description` | `og:description`, `twitter:description`, `description`, JSON-LD |
| `images` | every `og:image` with width, height, alt, type; then `twitter:image`, `image_src`, JSON-LD |
| `videos`, `audios` | `og:video`, `og:audio` |
| `favicon`, `icons` | the largest declared icon, or `/favicon.ico` |
| `site_name` | `og:site_name`, `application-name` |
| `canonical` | `<link rel=canonical>`, `og:url` |
| `feeds` | RSS, Atom and JSON Feed links |
| `oembed` | the page's JSON oEmbed endpoint |
| `schema` | the main JSON-LD entity: type, name, description, image, author, date, price |
| `embed` | the oEmbed answer: title, author, thumbnail, provider and embed HTML |
| `locale`, `kind`, `author`, `published`, `keywords`, `theme_color`, `twitter` | the matching tags |

Pages in GBK, Big5, Shift_JIS, EUC-KR, Windows-1251 and every other WHATWG encoding are
decoded. A URL that is an image comes back as a page whose image is itself. A page whose
head says nothing is searched further, and a `<meta http-equiv=refresh>` placeholder is
followed once.

## oEmbed

For these sites ogpeek asks the official oEmbed endpoint at the same time as the page,
and the provider's answer wins for title, image and author. Their pages are often a login
wall or an empty script shell; the endpoints answer anyway.

YouTube, Vimeo, TikTok, Spotify, SoundCloud, Mixcloud, X (posts), Bluesky (posts),
Flickr, Dailymotion, TED, Giphy, Loom.

Each was checked against real URLs from a server; none needs an API key. Providers that
need a token (Instagram, Facebook, Threads) or that answer servers with a block page are
not listed. Any other page that advertises an endpoint with
`<link rel="alternate" type="application/json+oembed">` is asked when its own tags lack a
title or image. `ogpeek::oembed_endpoint` and `ogpeek::parse_oembed` work on their own.

`embed.html` is the provider's embed code. Show it only in a sandboxed iframe.

## JSON-LD

`<script type="application/ld+json">` blocks in the head are read, including `@graph`
and a page's `mainEntity`. The subject is chosen by type (article, product, video, person
and so on outrank the website and organization), by whether its `url` is this page, and
against types repeated as a list (a profile's posts). Blocks placed after `</head>` are not
read; reaching them would mean downloading the page.

## Only the parser

Turn off default features to get the parser without reqwest or tokio:

```toml
ogpeek = { version = "0.1", default-features = false }
```

```rust
let page = ogpeek::parse(html, "https://example.com/post");
let page = ogpeek::parse_bytes(bytes, "https://example.com/post", Some("text/html; charset=gbk"));
```

## Settings

```rust
let peeker = ogpeek::Peeker::builder()
    .user_agent("MyApp/1.0 (+https://example.com)")
    .timeout(Duration::from_secs(5))
    .cache_ttl(Duration::from_secs(3600))
    .accept_language("ja,en;q=0.8")
    .host_header("api.github.com", "authorization", "Bearer …") // sent to that host only
    .build()?;
```

`Peeker::fetch` and `Peeker::fetch_head` give raw bytes through the same protections, for
the feeds, oEmbed JSON and images that go with a preview. `ogpeek::Resolver` is the
resolver on its own, for your other reqwest clients.

## Command line

```sh
cargo install ogpeek --features cli
ogpeek https://www.rust-lang.org https://github.com/rust-lang/rust
```

Prints each preview as JSON with the time it took.

## Features

| Feature | Default | |
|---|---|---|
| `fetch` | yes | `Peeker`, the safe resolver and errors (reqwest, tokio) |
| `serde` | no | `Serialize` and `Deserialize` for `Page` |
| `cli` | no | the `ogpeek` binary |

Minimum Rust version: 1.88.

## License

MIT or Apache-2.0, at your option.

ogpeek is built and used by [Bioly](https://bioly.link) for the link cards on bio pages.
