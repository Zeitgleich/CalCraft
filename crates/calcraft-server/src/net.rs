//! Client addresses: CIDR ranges, trusted reverse proxies and
//! `X-Forwarded-For`.
//!
//! The client of a request is the TCP peer, unless the peer is one of the
//! configured trusted proxies (`server.trusted_proxies`, empty by default).
//! Only then `X-Forwarded-For` is read, from right to left: every proxy
//! appends the address it received the request from, so the first entry
//! that is not a trusted proxy is the client. Entries further left were
//! written by the client itself and are never looked at. The `Forwarded`
//! header (RFC 7239) is not evaluated.

use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    str::FromStr,
};

use axum::http::{HeaderMap, HeaderName};

/// Header set by reverse proxies (nginx `$proxy_add_x_forwarded_for`,
/// Apache `mod_proxy`).
pub const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");

/// Most `X-Forwarded-For` entries examined (from the right). Longer proxy
/// chains are not plausible; the cap bounds the work per request.
pub const MAX_FORWARDED_HOPS: usize = 16;

/// An IP network such as `192.0.2.0/24` or `2001:db8::/32`. A bare
/// address is a single host. Host bits must be zero, so that a typo like
/// `10.0.0.1/8` is reported instead of silently trusting a whole network.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cidr {
    network: IpAddr,
    prefix: u8,
}

/// Same as `Display`, so that `check-config` prints `127.0.0.1/32`.
impl fmt::Debug for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid CIDR range {0:?} (expected e.g. 192.0.2.0/24, 2001:db8::/32 or a single address)")]
pub struct CidrError(String);

impl Cidr {
    fn new(network: IpAddr, prefix: u8) -> Option<Self> {
        let network = network.to_canonical();
        if prefix > max_prefix(network) || mask(network, prefix) != network {
            return None;
        }
        Some(Self { network, prefix })
    }

    /// `127.0.0.1/32` and `::1/128`: the default of
    /// `server.trusted_proxies`, for a reverse proxy on the same host.
    pub fn loopback() -> Vec<Self> {
        vec![
            Self {
                network: IpAddr::V4(Ipv4Addr::LOCALHOST),
                prefix: 32,
            },
            Self {
                network: IpAddr::V6(Ipv6Addr::LOCALHOST),
                prefix: 128,
            },
        ]
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        same_family(ip, self.network) && mask(ip, self.prefix) == self.network
    }
}

impl FromStr for Cidr {
    type Err = CidrError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let error = || CidrError(text.chars().take(64).collect());
        let (address, prefix) = match text.split_once('/') {
            Some((address, prefix)) => {
                // Plain decimal without leading zeros (`/024` is rejected).
                if prefix.is_empty()
                    || prefix.len() > 3
                    || (prefix.len() > 1 && prefix.starts_with('0'))
                    || !prefix.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(error());
                }
                (address, Some(prefix.parse::<u8>().map_err(|_| error())?))
            }
            None => (text, None),
        };
        // `IpAddr::from_str` rejects surrounding whitespace, zone ids and ports.
        let address: IpAddr = address.parse().map_err(|_| error())?;
        // A mapped address (`::ffff:192.0.2.1/128`) is matched as IPv4.
        let (address, prefix) = match (address, prefix) {
            (IpAddr::V6(v6), Some(prefix)) if v6.to_ipv4_mapped().is_some() => {
                (address, prefix.checked_sub(96).ok_or_else(error)?)
            }
            (_, Some(prefix)) => (address, prefix),
            (address, None) => (address, max_prefix(address.to_canonical())),
        };
        Self::new(address, prefix).ok_or_else(error)
    }
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

impl serde::Serialize for Cidr {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for Cidr {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

fn max_prefix(ip: IpAddr) -> u8 {
    match ip {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    }
}

fn same_family(a: IpAddr, b: IpAddr) -> bool {
    matches!(
        (a, b),
        (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_))
    )
}

/// Keeps the first `prefix` bits of `ip`.
fn mask(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            let mask = u32::MAX
                .checked_shl(32 - u32::from(prefix.min(32)))
                .unwrap_or(0);
            IpAddr::V4(Ipv4Addr::from(bits & mask))
        }
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            let mask = u128::MAX
                .checked_shl(128 - u32::from(prefix.min(128)))
                .unwrap_or(0);
            IpAddr::V6(Ipv6Addr::from(bits & mask))
        }
    }
}

/// Whether `ip` lies in one of `ranges`.
pub fn is_trusted(ranges: &[Cidr], ip: IpAddr) -> bool {
    ranges.iter().any(|range| range.contains(ip))
}

/// The client address of a request from `peer`.
///
/// `X-Forwarded-For` is only used when `peer` is a trusted proxy. Walking
/// right to left, trusted proxies are skipped and the first other address
/// is the client. A malformed entry ends the walk at the last proxy seen,
/// as nothing left of it can be attributed reliably; so does the hop cap.
pub fn client_ip(peer: IpAddr, headers: &HeaderMap, trusted: &[Cidr]) -> IpAddr {
    let peer = peer.to_canonical();
    if !is_trusted(trusted, peer) {
        return peer;
    }
    let mut client = peer;
    // Several header lines form one list, in order (RFC 9110 §5.3).
    let values: Vec<&[u8]> = headers
        .get_all(X_FORWARDED_FOR)
        .iter()
        .map(|value| value.as_bytes())
        .collect();
    let entries = values
        .iter()
        .rev()
        .flat_map(|value| value.rsplit(|&b| b == b','))
        .take(MAX_FORWARDED_HOPS);
    for entry in entries {
        let Some(ip) = parse_forwarded_ip(entry) else {
            return client;
        };
        client = ip;
        if !is_trusted(trusted, ip) {
            return ip;
        }
    }
    client
}

/// Parses one `X-Forwarded-For` entry: an IPv4 or IPv6 address, optionally
/// with a port (`192.0.2.1:1234`, `[2001:db8::1]:1234`), as some proxies
/// add it.
fn parse_forwarded_ip(entry: &[u8]) -> Option<IpAddr> {
    let entry = std::str::from_utf8(entry).ok()?.trim_matches([' ', '\t']);
    if entry.len() > 64 {
        return None;
    }
    if let Ok(ip) = entry.parse::<IpAddr>() {
        return Some(ip.to_canonical());
    }
    if let Some(rest) = entry.strip_prefix('[') {
        let (address, after) = rest.split_once(']')?;
        if !(after.is_empty() || after.strip_prefix(':').is_some_and(valid_port)) {
            return None;
        }
        return address
            .parse::<Ipv6Addr>()
            .ok()
            .map(|ip| IpAddr::V6(ip).to_canonical());
    }
    let (address, port) = entry.split_once(':')?;
    valid_port(port)
        .then(|| address.parse::<Ipv4Addr>().ok())
        .flatten()
        .map(IpAddr::V4)
}

fn valid_port(port: &str) -> bool {
    !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit())
}

/// The key under which per-client limits are tracked: the IPv4 address, or
/// the IPv6 network of `ipv6_prefix` bits (a single host usually controls a
/// whole /64).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientKey(IpAddr);

impl ClientKey {
    pub fn new(ip: IpAddr, ipv6_prefix: u8) -> Self {
        let ip = ip.to_canonical();
        match ip {
            IpAddr::V4(_) => Self(ip),
            IpAddr::V6(_) => Self(mask(ip, ipv6_prefix)),
        }
    }
}

impl fmt::Display for ClientKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The resolved client of a request, stored as request extension by the
/// middleware in [`crate::limits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp {
    pub ip: IpAddr,
    pub key: ClientKey,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cidr(text: &str) -> Cidr {
        text.parse().unwrap()
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn parses_and_matches_cidr() {
        let net = cidr("192.0.2.0/24");
        assert!(net.contains(ip("192.0.2.0")));
        assert!(net.contains(ip("192.0.2.255")));
        assert!(!net.contains(ip("192.0.3.0")));
        assert!(!net.contains(ip("2001:db8::1")));
        // IPv4-mapped IPv6 peers (dual-stack sockets) match IPv4 ranges.
        assert!(net.contains(ip("::ffff:192.0.2.7")));

        let host = cidr("127.0.0.1");
        assert_eq!(host.to_string(), "127.0.0.1/32");
        assert!(host.contains(ip("127.0.0.1")));
        assert!(!host.contains(ip("127.0.0.2")));

        let v6 = cidr("2001:db8::/32");
        assert!(v6.contains(ip("2001:db8:ffff::1")));
        assert!(!v6.contains(ip("2001:db9::1")));
        assert!(cidr("::1/128").contains(ip("::1")));
        assert!(cidr("0.0.0.0/0").contains(ip("203.0.113.9")));
        assert!(!cidr("0.0.0.0/0").contains(ip("::1")));
        assert!(cidr("::/0").contains(ip("2001:db8::1")));
        assert_eq!(cidr("::ffff:10.0.0.0/104"), cidr("10.0.0.0/8"));
        assert_eq!(Cidr::loopback(), [cidr("127.0.0.1/32"), cidr("::1/128")]);
    }

    #[test]
    fn rejects_invalid_cidr() {
        for bad in [
            "",
            "/24",
            "192.0.2.0/",
            "192.0.2.0/33",
            "2001:db8::/129",
            "192.0.2.1/24",
            "10.0.0.1/8",
            "192.0.2.0/+8",
            "192.0.2.0/0024",
            "192.0.2.0/024",
            "192.0.2.0/00",
            " 192.0.2.0/24",
            "192.0.2.0/24 ",
            "192.0.2.0/24/8",
            "fe80::1%eth0/128",
            "localhost",
            "192.0.2.0:80",
            "::ffff:10.0.0.0/8",
        ] {
            assert!(bad.parse::<Cidr>().is_err(), "{bad:?}");
        }
    }

    fn headers(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(X_FORWARDED_FOR, value.parse().unwrap());
        }
        headers
    }

    fn resolve(peer: &str, xff: &[&str], trusted: &[&str]) -> IpAddr {
        let trusted: Vec<Cidr> = trusted.iter().map(|t| cidr(t)).collect();
        client_ip(ip(peer), &headers(xff), &trusted)
    }

    #[test]
    fn forwarded_for_is_ignored_from_untrusted_peers() {
        assert_eq!(
            resolve("203.0.113.5", &["198.51.100.1"], &[]),
            ip("203.0.113.5")
        );
        assert_eq!(
            resolve("203.0.113.5", &["198.51.100.1"], &["127.0.0.1"]),
            ip("203.0.113.5")
        );
        assert_eq!(resolve("::ffff:203.0.113.5", &[], &[]), ip("203.0.113.5"));
    }

    #[test]
    fn forwarded_for_is_honoured_from_trusted_peers() {
        let trusted = ["127.0.0.1/32", "::1/128", "10.0.0.0/8"];
        assert_eq!(
            resolve("127.0.0.1", &["198.51.100.1"], &trusted),
            ip("198.51.100.1")
        );
        // Chain of trusted proxies: skip them.
        assert_eq!(
            resolve("127.0.0.1", &["198.51.100.1, 10.1.2.3"], &trusted),
            ip("198.51.100.1")
        );
        // Several header lines are one list.
        assert_eq!(
            resolve("::1", &["198.51.100.1", "10.1.2.3"], &trusted),
            ip("198.51.100.1")
        );
        // Without the header the proxy itself is the client.
        assert_eq!(resolve("127.0.0.1", &[], &trusted), ip("127.0.0.1"));
        // Mapped peer address of a dual-stack socket.
        assert_eq!(
            resolve("::ffff:127.0.0.1", &["2001:db8::7"], &trusted),
            ip("2001:db8::7")
        );
        // Ports are tolerated.
        assert_eq!(
            resolve("127.0.0.1", &["198.51.100.1:4711"], &trusted),
            ip("198.51.100.1")
        );
        assert_eq!(
            resolve("127.0.0.1", &["[2001:db8::7]:4711"], &trusted),
            ip("2001:db8::7")
        );
    }

    #[test]
    fn spoofed_entries_are_ignored() {
        let trusted = ["127.0.0.1/32"];
        // The client sent "X-Forwarded-For: 1.2.3.4"; nginx appended its peer.
        assert_eq!(
            resolve("127.0.0.1", &["1.2.3.4, 203.0.113.9"], &trusted),
            ip("203.0.113.9")
        );
        // Spoofing a trusted address on the left does not help either.
        assert_eq!(
            resolve("127.0.0.1", &["127.0.0.1, 203.0.113.9"], &trusted),
            ip("203.0.113.9")
        );
        // Garbage left of the real client is never parsed.
        assert_eq!(
            resolve("127.0.0.1", &["<script>, , unknown, 203.0.113.9"], &trusted),
            ip("203.0.113.9")
        );
    }

    #[test]
    fn malformed_or_long_lists_stop_at_the_last_proxy() {
        let trusted = ["127.0.0.1/32", "10.0.0.0/8"];
        for bad in [
            "unknown",
            "",
            "999.1.1.1",
            "1.2.3.4:x",
            "[::1",
            "_hidden",
            "1.2.3.4 5",
        ] {
            assert_eq!(
                resolve("127.0.0.1", &[bad], &trusted),
                ip("127.0.0.1"),
                "{bad:?}"
            );
        }
        assert_eq!(
            resolve("127.0.0.1", &["203.0.113.9, garbage, 10.0.0.2"], &trusted),
            ip("10.0.0.2")
        );
        // Only MAX_FORWARDED_HOPS entries are examined.
        let many = vec!["10.0.0.1"; 1000].join(",");
        let list = format!("203.0.113.9,{many}");
        assert_eq!(resolve("127.0.0.1", &[&list], &trusted), ip("10.0.0.1"));
        let short = format!(
            "203.0.113.9,{}",
            vec!["10.0.0.1"; MAX_FORWARDED_HOPS - 1].join(",")
        );
        assert_eq!(resolve("127.0.0.1", &[&short], &trusted), ip("203.0.113.9"));
    }

    #[test]
    fn ipv6_clients_are_grouped_by_prefix() {
        let a = ClientKey::new(ip("2001:db8:1:2:aaaa::1"), 64);
        let b = ClientKey::new(ip("2001:db8:1:2:bbbb::1"), 64);
        let c = ClientKey::new(ip("2001:db8:1:3::1"), 64);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(
            ClientKey::new(ip("2001:db8::1"), 128),
            ClientKey::new(ip("2001:db8::2"), 128)
        );
        assert_ne!(
            ClientKey::new(ip("192.0.2.1"), 64),
            ClientKey::new(ip("192.0.2.2"), 64)
        );
        assert_eq!(
            ClientKey::new(ip("::ffff:192.0.2.1"), 64),
            ClientKey::new(ip("192.0.2.1"), 64)
        );
    }
}
