//! Safe outbound HTTP: address checks, DNS, and the error type.

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Why a peek or fetch failed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// Not an absolute http or https URL, or it carries a user name or password.
    InvalidUrl,
    /// The host resolves to a private, loopback, link-local or otherwise internal address,
    /// or the URL names a non-default port. See [`forbidden_ip`].
    Blocked,
    /// The server answered with this non-success status.
    Status(u16),
    TooManyRedirects,
    Timeout,
    /// Could not connect (DNS failure, refused, TLS error).
    Connect,
    /// Anything else that went wrong on the wire.
    Network(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidUrl => f.write_str("invalid url"),
            Error::Blocked => f.write_str("blocked address"),
            Error::Status(s) => write!(f, "http status {s}"),
            Error::TooManyRedirects => f.write_str("too many redirects"),
            Error::Timeout => f.write_str("timed out"),
            Error::Connect => f.write_str("could not connect"),
            Error::Network(e) => write!(f, "network error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        let mut src: Option<&(dyn std::error::Error + 'static)> = Some(&e);
        while let Some(x) = src {
            if x.is::<BlockedHost>() {
                return Error::Blocked;
            }
            src = x.source();
        }
        if e.is_timeout() {
            Error::Timeout
        } else if e.is_connect() {
            Error::Connect
        } else {
            Error::Network(e.to_string())
        }
    }
}

/// True for addresses a server-side fetcher must never connect to: loopback, private
/// ranges, link-local (including cloud metadata at 169.254.169.254), carrier-grade NAT,
/// multicast, documentation and reserved ranges, and the IPv6 forms that embed one of
/// those IPv4 addresses (mapped, compatible, NAT64, 6to4).
pub fn forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_broadcast()
                || v.is_documentation()
                || v.is_unspecified()
                || v.is_multicast()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1]))
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || (o[0] == 198 && (18..20).contains(&o[1]))
                || o[0] >= 240
        }
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return forbidden_ip(IpAddr::V4(v4));
            }
            let s = v.segments();
            let embedded = |hi: u16, lo: u16| {
                forbidden_ip(IpAddr::V4(Ipv4Addr::from(
                    (u32::from(hi) << 16) | u32::from(lo),
                )))
            };
            if s[..6] == [0, 0, 0, 0, 0, 0] && !v.is_loopback() && !v.is_unspecified() {
                return embedded(s[6], s[7]);
            }
            if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                return embedded(s[6], s[7]);
            }
            if s[0] == 0x2002 {
                return embedded(s[1], s[2]);
            }
            v.is_loopback()
                || v.is_unspecified()
                || v.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] & 0xffc0) == 0xfec0
                || (s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1)
                || (s[0] == 0x2001 && s[1] == 0x0db8)
                || (s[0] == 0x0100 && s[1..4] == [0, 0, 0])
        }
    }
}

#[derive(Debug)]
pub(crate) struct BlockedHost;
impl fmt::Display for BlockedHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("host resolves to a blocked address")
    }
}
impl std::error::Error for BlockedHost {}

const DNS_CAP: usize = 4096;

/// A DNS resolver for reqwest that caches answers, puts IPv4 first, and (when blocking)
/// refuses any host with an internal address.
///
/// The addresses that are checked are the addresses that get connected to, so a second
/// lookup cannot swap in an internal address (DNS rebinding). IPv4 goes first because
/// many container networks have no IPv6 route, where trying IPv6 first costs a timeout.
///
/// ```no_run
/// let client = reqwest::Client::builder()
///     .dns_resolver(std::sync::Arc::new(ogpeek::Resolver::prefer_ipv4()))
///     .build()?;
/// # Ok::<_, reqwest::Error>(())
/// ```
#[derive(Clone)]
pub struct Resolver {
    inner: Arc<ResolverInner>,
}

type Answer = (Instant, Arc<[SocketAddr]>);

struct ResolverInner {
    block: bool,
    ttl: Duration,
    cache: Mutex<HashMap<String, Answer>>,
}

impl Resolver {
    /// Refuses hosts with any internal address. This is what [`crate::Peeker`] uses.
    pub fn safe() -> Self {
        Self::new(true)
    }

    /// Only caches and sorts. For fixed, trusted API hosts.
    pub fn prefer_ipv4() -> Self {
        Self::new(false)
    }

    fn new(block: bool) -> Self {
        Resolver {
            inner: Arc::new(ResolverInner {
                block,
                ttl: Duration::from_secs(60),
                cache: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// How long answers are kept. Default 60 seconds; zero turns the cache off.
    pub fn with_ttl(self, ttl: Duration) -> Self {
        Resolver {
            inner: Arc::new(ResolverInner {
                block: self.inner.block,
                ttl,
                cache: Mutex::new(HashMap::new()),
            }),
        }
    }

    async fn lookup(inner: Arc<ResolverInner>, host: String) -> std::io::Result<Arc<[SocketAddr]>> {
        if let Some((at, addrs)) = inner.cache.lock().expect("dns cache").get(&host)
            && at.elapsed() < inner.ttl
        {
            return Ok(addrs.clone());
        }
        let mut addrs: Vec<SocketAddr> =
            tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
        addrs.sort_by_key(SocketAddr::is_ipv6);
        let addrs: Arc<[SocketAddr]> = addrs.into();
        if !inner.ttl.is_zero() {
            let mut cache = inner.cache.lock().expect("dns cache");
            if cache.len() >= DNS_CAP {
                cache.retain(|_, (at, _)| at.elapsed() < inner.ttl);
                if cache.len() >= DNS_CAP {
                    cache.clear();
                }
            }
            cache.insert(host, (Instant::now(), addrs.clone()));
        }
        Ok(addrs)
    }
}

impl reqwest::dns::Resolve for Resolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let inner = self.inner.clone();
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let block = inner.block;
            let addrs = Resolver::lookup(inner, host).await?;
            if addrs.is_empty() || (block && addrs.iter().any(|a| forbidden_ip(a.ip()))) {
                return Err(Box::new(BlockedHost) as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new((0..addrs.len()).map(move |i| addrs[i])) as reqwest::dns::Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::forbidden_ip;

    #[test]
    fn blocks_internal_addresses() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::127.0.0.1",
            "64:ff9b::a00:1",
            "2002:7f00:1::",
        ] {
            assert!(forbidden_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["1.1.1.1", "93.184.216.34", "2606:4700::1111"] {
            assert!(!forbidden_ip(ip.parse().unwrap()), "{ip}");
        }
    }
}
