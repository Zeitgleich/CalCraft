//! Which feed URLs and addresses may be fetched (SSRF protection).
//!
//! Lessons from earlier calendar servers (docs/security/angriffshistorie.md:
//! Nextcloud CVE-2020-8118/-8138, OX App Suite, MagicMirror, plone):
//!
//! * Only `https` (and `webcal`, which means https); `http` only if the
//!   administrator allows it. No user info, no unusual ports, bounded length.
//! * Host names must be plain ASCII DNS names with at least two labels and
//!   a non-numeric top-level label, so `2130706433`, `0x7f.1`, `127.1` and
//!   friends never reach a resolver that would read them as addresses.
//!   Internationalised names must be given in their `xn--` form.
//! * Every address a name resolves to must be public ([`is_public`]); the
//!   fetcher then connects to exactly that address (pinning).
//! * IPv4-mapped IPv6 addresses are judged by their IPv4 address; all other
//!   transition forms (6to4, Teredo, NAT64, IPv4-compatible) are refused.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use super::SubscriptionError;
use crate::config::SubscriptionsConfig;

/// Longest accepted feed URL.
pub const MAX_URL_LEN: usize = 2048;
const MAX_HOST_LEN: usize = 253;

/// Cloud metadata services on otherwise public addresses (the link-local
/// and unique-local ones, e.g. `169.254.169.254` and `fd00:ec2::254`, are
/// covered by the ranges).
const METADATA_V4: [Ipv4Addr; 1] = [
    // Azure WireServer.
    Ipv4Addr::new(168, 63, 129, 16),
];

/// Whether `ip` is an ordinary public unicast address a feed may live on.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(v6),
        },
    }
}

fn in_v4(ip: Ipv4Addr, net: [u8; 4], prefix: u32) -> bool {
    let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
    u32::from(ip) & mask == u32::from(Ipv4Addr::from(net)) & mask
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    const BLOCKED: &[([u8; 4], u32)] = &[
        ([0, 0, 0, 0], 8),       // "this network", 0.0.0.0
        ([10, 0, 0, 0], 8),      // private
        ([100, 64, 0, 0], 10),   // carrier-grade NAT (also Alibaba metadata)
        ([127, 0, 0, 0], 8),     // loopback
        ([169, 254, 0, 0], 16),  // link-local, cloud metadata
        ([172, 16, 0, 0], 12),   // private
        ([192, 0, 0, 0], 24),    // IETF protocol assignments (Oracle metadata)
        ([192, 0, 2, 0], 24),    // documentation
        ([192, 88, 99, 0], 24),  // 6to4 relay anycast
        ([192, 168, 0, 0], 16),  // private
        ([198, 18, 0, 0], 15),   // benchmarking
        ([198, 51, 100, 0], 24), // documentation
        ([203, 0, 113, 0], 24),  // documentation
        ([224, 0, 0, 0], 4),     // multicast
        ([240, 0, 0, 0], 4),     // reserved, broadcast
    ];
    !BLOCKED.iter().any(|(net, prefix)| in_v4(ip, *net, *prefix)) && !METADATA_V4.contains(&ip)
}

fn in_v6(ip: Ipv6Addr, net: [u16; 8], prefix: u32) -> bool {
    let mask = u128::MAX.checked_shl(128 - prefix).unwrap_or(0);
    u128::from(ip) & mask == u128::from(Ipv6Addr::from(net)) & mask
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    // Only global unicast (2000::/3) is ever public. This alone excludes
    // ::, ::1, IPv4-compatible ::/96, NAT64 64:ff9b::/96 and 64:ff9b:1::/48,
    // discard 100::/64, ULA fc00::/7 (fd00:ec2::254), link-local fe80::/10,
    // site-local fec0::/10 and multicast ff00::/8.
    const BLOCKED: &[([u16; 8], u32)] = &[
        ([0x2001, 0, 0, 0, 0, 0, 0, 0], 23), // IETF protocol assignments, Teredo 2001::/32
        ([0x2001, 0xdb8, 0, 0, 0, 0, 0, 0], 32), // documentation
        ([0x2002, 0, 0, 0, 0, 0, 0, 0], 16), // 6to4 (embeds any IPv4 address)
        ([0x3fff, 0, 0, 0, 0, 0, 0, 0], 20), // documentation (RFC 9637)
    ];
    in_v6(ip, [0x2000, 0, 0, 0, 0, 0, 0, 0], 3)
        && !BLOCKED.iter().any(|(net, prefix)| in_v6(ip, *net, *prefix))
}

/// The host of a feed URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    /// A DNS name, lower case, without trailing dot.
    Name(String),
    /// A public address literal.
    Ip(IpAddr),
}

/// A feed URL that passed [`Policy::parse_url`].
#[derive(Clone, PartialEq, Eq)]
pub struct FeedUrl {
    pub https: bool,
    pub host: Host,
    pub port: u16,
    /// Path and query, starting with `/`.
    pub path_and_query: String,
}

/// Never prints path or query: feed URLs often carry secret tokens.
impl std::fmt::Debug for FeedUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeedUrl")
            .field("https", &self.https)
            .field("host", &self.host_name())
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl FeedUrl {
    fn default_port(&self) -> u16 {
        if self.https { 443 } else { 80 }
    }

    /// The host for logs and the `Host` header, without port. IPv6
    /// addresses in brackets.
    pub fn host_name(&self) -> String {
        match &self.host {
            Host::Name(name) => name.clone(),
            Host::Ip(IpAddr::V6(v6)) => format!("[{v6}]"),
            Host::Ip(ip) => ip.to_string(),
        }
    }

    /// Name the TLS certificate must be valid for.
    pub fn server_name(&self) -> String {
        match &self.host {
            Host::Name(name) => name.clone(),
            Host::Ip(ip) => ip.to_string(),
        }
    }

    /// `Host` header value.
    pub fn host_header(&self) -> String {
        if self.port == self.default_port() {
            self.host_name()
        } else {
            format!("{}:{}", self.host_name(), self.port)
        }
    }

    /// The normalised URL (stored and shown to the owner only).
    pub fn as_string(&self) -> String {
        let scheme = if self.https { "https" } else { "http" };
        format!("{scheme}://{}{}", self.host_header(), self.path_and_query)
    }

    /// Resolves a `Location` header against this URL (RFC 3986 §5.2,
    /// without dot-segment removal, which the server does).
    pub fn join(&self, location: &str) -> Option<String> {
        let location = location.trim();
        if location.is_empty() || location.len() > MAX_URL_LEN {
            return None;
        }
        let lower = location.to_ascii_lowercase();
        if ["https:", "http:", "webcal:", "webcals:"]
            .iter()
            .any(|scheme| lower.starts_with(scheme))
        {
            return Some(location.to_owned());
        }
        let scheme = if self.https { "https:" } else { "http:" };
        if location.starts_with("//") {
            return Some(format!("{scheme}{location}"));
        }
        // Any other scheme ("ftp:", "file:", "javascript:") is refused later.
        if location
            .split(['/', '?', '#'])
            .next()
            .is_some_and(|first| first.contains(':'))
        {
            return Some(location.to_owned());
        }
        let origin = format!("{scheme}//{}", self.host_header());
        if location.starts_with('/') {
            return Some(format!("{origin}{location}"));
        }
        let path = self.path_and_query.split('?').next().unwrap_or("/");
        if location.starts_with('?') {
            return Some(format!("{origin}{path}{location}"));
        }
        let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
        let dir = if dir.is_empty() { "/" } else { dir };
        Some(format!("{origin}{dir}{location}"))
    }
}

/// `example.com` (exactly) or `*.example.com` / `.example.com` (any
/// subdomain, not the name itself).
#[derive(Debug, Clone, PartialEq, Eq)]
enum HostPattern {
    Exact(String),
    Suffix(String),
}

impl HostPattern {
    fn new(pattern: &str) -> Self {
        let pattern = pattern.to_ascii_lowercase();
        match pattern
            .strip_prefix("*.")
            .or_else(|| pattern.strip_prefix('.'))
        {
            Some(name) => Self::Suffix(format!(".{name}")),
            None => Self::Exact(pattern),
        }
    }

    fn matches(&self, host: &str) -> bool {
        match self {
            Self::Exact(name) => host == name,
            Self::Suffix(suffix) => host.ends_with(suffix.as_str()),
        }
    }
}

/// Names that resolve to the local machine or network by convention.
const LOCAL_SUFFIXES: [&str; 6] = [
    "localhost",
    "local",
    "localdomain",
    "internal",
    "home.arpa",
    "arpa",
];

fn invalid(reason: &'static str) -> SubscriptionError {
    SubscriptionError::InvalidUrl(reason)
}

/// Validates a DNS host name; returns it in lower case without the
/// trailing dot.
fn host_name(host: &str) -> Result<String, SubscriptionError> {
    if !host.is_ascii() {
        return Err(invalid("use the xn-- form of internationalised host names"));
    }
    let host = host.to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host).to_owned();
    if host.is_empty() || host.len() > MAX_HOST_LEN {
        return Err(invalid("invalid host"));
    }
    let labels: Vec<&str> = host.split('.').collect();
    let valid_label = |label: &&str| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    };
    if !labels.iter().all(valid_label) {
        return Err(invalid("invalid host"));
    }
    if LOCAL_SUFFIXES
        .iter()
        .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
    {
        return Err(SubscriptionError::BlockedAddress);
    }
    // Single-label names resolve through search domains (internal hosts).
    let Some(tld) = labels.last().filter(|_| labels.len() >= 2) else {
        return Err(invalid("host must be a fully qualified domain name"));
    };
    // Numeric forms like 127.1, 0x7f.0.0.1 or 2130706433 are addresses to
    // some resolvers; real top-level domains are never numeric.
    let numeric = tld.bytes().all(|b| b.is_ascii_digit())
        || (tld.starts_with("0x") && tld[2..].bytes().all(|b| b.is_ascii_hexdigit()));
    if numeric {
        return Err(invalid("invalid host"));
    }
    Ok(host)
}

/// The rules a feed URL and its addresses must follow.
#[derive(Debug, Clone)]
pub struct Policy {
    allow_http: bool,
    ports: Vec<u16>,
    allowed_hosts: Vec<HostPattern>,
    denied_hosts: Vec<HostPattern>,
    /// Addresses treated as public, for tests with a local server only.
    /// Never set outside `#[cfg(test)]` code.
    #[cfg(test)]
    pub(crate) test_exempt: Vec<IpAddr>,
}

impl Policy {
    pub fn new(config: &SubscriptionsConfig) -> Self {
        Self {
            allow_http: config.allow_http,
            ports: config.ports(),
            allowed_hosts: config
                .allowed_hosts
                .iter()
                .map(|h| HostPattern::new(h))
                .collect(),
            denied_hosts: config
                .denied_hosts
                .iter()
                .map(|h| HostPattern::new(h))
                .collect(),
            #[cfg(test)]
            test_exempt: Vec::new(),
        }
    }

    /// Whether `ip` may be connected to; returns the address to use
    /// (IPv4-mapped addresses as plain IPv4).
    pub fn check_ip(&self, ip: IpAddr) -> Result<IpAddr, SubscriptionError> {
        let ip = ip.to_canonical();
        #[cfg(test)]
        if self.test_exempt.contains(&ip) {
            return Ok(ip);
        }
        if is_public(ip) {
            Ok(ip)
        } else {
            Err(SubscriptionError::BlockedAddress)
        }
    }

    fn check_host(&self, host: &str) -> Result<(), SubscriptionError> {
        if self.denied_hosts.iter().any(|p| p.matches(host))
            || (!self.allowed_hosts.is_empty()
                && !self.allowed_hosts.iter().any(|p| p.matches(host)))
        {
            return Err(SubscriptionError::BlockedAddress);
        }
        Ok(())
    }

    /// Parses and checks a feed URL (also every redirect target).
    /// `webcal://` and `webcals://` become `https://`. Address literals
    /// must be public; names are checked once resolved.
    pub fn parse_url(&self, input: &str) -> Result<FeedUrl, SubscriptionError> {
        let input = input.trim();
        if input.is_empty() || input.len() > MAX_URL_LEN {
            return Err(invalid("URL is empty or too long"));
        }
        if input
            .bytes()
            .any(|b| b.is_ascii_control() || b == b' ' || b == b'\\')
        {
            return Err(invalid("URL contains spaces or control characters"));
        }
        // The fragment is never sent.
        let input = input.split('#').next().unwrap_or_default();
        let (scheme, rest) = input
            .split_once("://")
            .ok_or(invalid("not an absolute URL"))?;
        let https = match scheme.to_ascii_lowercase().as_str() {
            "https" | "webcal" | "webcals" => true,
            "http" if self.allow_http => false,
            "http" => return Err(invalid("only https URLs are allowed")),
            _ => return Err(invalid("only https and webcal URLs are allowed")),
        };
        let split = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, path) = rest.split_at(split);
        if authority.contains('@') {
            return Err(invalid("URLs with user name or password are not allowed"));
        }
        if authority.contains('%') {
            return Err(invalid("invalid host"));
        }
        let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
            let (literal, after) = bracketed.split_once(']').ok_or(invalid("invalid host"))?;
            let ip: Ipv6Addr = literal.parse().map_err(|_| invalid("invalid host"))?;
            (Host::Ip(IpAddr::V6(ip)), after)
        } else {
            let (host, after) = match authority.find(':') {
                Some(at) => authority.split_at(at),
                None => (authority, ""),
            };
            // Only the canonical dotted quad is an IPv4 literal; anything
            // else numeric is refused by `host_name`.
            match host.parse::<Ipv4Addr>() {
                Ok(ip) => (Host::Ip(IpAddr::V4(ip)), after),
                Err(_) => (Host::Name(host_name(host)?), after),
            }
        };
        // Addresses and host lists first: "not allowed" is the more useful
        // answer for a private address on an odd port.
        match &host {
            Host::Ip(ip) => {
                self.check_ip(*ip)?;
                self.check_host(&ip.to_string())?;
            }
            Host::Name(name) => self.check_host(name)?,
        }
        let port = match port {
            "" => {
                if https {
                    443
                } else {
                    80
                }
            }
            other => {
                let digits = other.strip_prefix(':').ok_or(invalid("invalid host"))?;
                if digits.is_empty()
                    || digits.len() > 5
                    || !digits.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(invalid("invalid port"));
                }
                digits.parse::<u16>().map_err(|_| invalid("invalid port"))?
            }
        };
        if !self.ports.contains(&port) {
            return Err(invalid("port not allowed"));
        }
        let path_and_query = if path.is_empty() {
            "/".to_owned()
        } else if path.starts_with('?') {
            format!("/{path}")
        } else {
            path.to_owned()
        };
        // Only characters allowed in a request target (RFC 3986), which
        // also keeps the URL valid as an `http::Uri`.
        let allowed = |b: u8| b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/?%[]".contains(&b);
        if !path_and_query.bytes().all(allowed) {
            return Err(invalid(
                "URL contains characters that must be percent-encoded",
            ));
        }
        Ok(FeedUrl {
            https,
            host,
            port,
            path_and_query,
        })
    }

    /// Checks a redirect target: the same rules as for the feed URL, and
    /// never from https down to http.
    pub fn redirect(&self, from: &FeedUrl, location: &str) -> Result<FeedUrl, SubscriptionError> {
        let target = from
            .join(location)
            .ok_or(SubscriptionError::FetchFailed(Some("invalid redirect")))?;
        let next = self.parse_url(&target).map_err(|err| match err {
            SubscriptionError::InvalidUrl(_) => {
                SubscriptionError::FetchFailed(Some("redirect to a URL that is not allowed"))
            }
            err => err,
        })?;
        if from.https && !next.https {
            return Err(SubscriptionError::FetchFailed(Some(
                "redirect from https to http",
            )));
        }
        Ok(next)
    }
}

#[cfg(test)]
mod tests;
