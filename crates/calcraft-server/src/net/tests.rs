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
