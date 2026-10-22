use super::*;

fn policy() -> Policy {
    Policy::new(&SubscriptionsConfig::default())
}

fn http_policy() -> Policy {
    Policy::new(&SubscriptionsConfig {
        allow_http: true,
        ..SubscriptionsConfig::default()
    })
}

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

#[test]
fn private_and_special_ipv4_ranges_are_blocked() {
    for blocked in [
        "0.0.0.0",
        "0.1.2.3",
        "10.0.0.1",
        "10.255.255.255",
        "100.64.0.1",
        "100.100.100.200", // Alibaba metadata
        "100.127.255.255",
        "127.0.0.1",
        "127.255.255.254",
        "169.254.0.1",
        "169.254.169.254", // AWS/GCP/Azure metadata
        "172.16.0.1",
        "172.31.255.255",
        "192.0.0.1",
        "192.0.0.192", // Oracle metadata
        "192.0.2.1",
        "192.88.99.1",
        "192.168.0.1",
        "192.168.255.255",
        "198.18.0.1",
        "198.19.255.255",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "239.255.255.250",
        "240.0.0.1",
        "255.255.255.255",
        "168.63.129.16", // Azure WireServer
    ] {
        assert!(!is_public(ip(blocked)), "{blocked} must be blocked");
    }
    for public in [
        "1.1.1.1",
        "8.8.8.8",
        "93.184.216.34",
        "100.63.255.255",
        "100.128.0.0",
        "172.15.255.255",
        "172.32.0.0",
        "192.0.1.1",
        "192.169.0.1",
        "198.17.255.255",
        "198.20.0.0",
        "223.255.255.255",
        "11.0.0.1",
        "126.255.255.255",
        "128.0.0.1",
    ] {
        assert!(is_public(ip(public)), "{public} must be allowed");
    }
}

#[test]
fn special_ipv6_ranges_are_blocked() {
    for blocked in [
        "::",
        "::1",
        "::ffff:127.0.0.1", // IPv4-mapped loopback (CVE-2020-8138)
        "::ffff:10.0.0.1",
        "::ffff:169.254.169.254",
        "::ffff:0.0.0.0",
        "::ffff:7f00:1", // mapped, hex form
        "::127.0.0.1",   // IPv4-compatible (deprecated)
        "::8.8.8.8",
        "64:ff9b::7f00:1", // NAT64 well-known prefix
        "64:ff9b::808:808",
        "64:ff9b:1::1", // NAT64 local use
        "100::1",       // discard
        "2001::1",      // Teredo
        "2001:0:4136:e378:8000:63bf:3fff:fdd2",
        "2001:2::1",       // benchmarking
        "2001:10::1",      // ORCHID
        "2001:db8::1",     // documentation
        "2002:7f00:1::1",  // 6to4 of 127.0.0.1
        "2002:808:808::1", // 6to4 of a public address: still refused
        "3fff::1",         // documentation (RFC 9637)
        "fc00::1",
        "fd00::1",
        "fd00:ec2::254", // AWS metadata (IPv6)
        "fe80::1",
        "fe80::1%1",
        "febf::1",
        "fec0::1", // site-local
        "ff02::1",
        "ff05::fb",
        "4000::1", // outside global unicast 2000::/3
        "e000::1",
    ] {
        let Ok(addr) = blocked.parse::<IpAddr>() else {
            // Zone indices are not addresses at all.
            assert!(blocked.contains('%'), "{blocked} did not parse");
            continue;
        };
        assert!(!is_public(addr), "{blocked} must be blocked");
    }
    for public in [
        "2606:4700:4700::1111",
        "2a00:1450:4001:82a::200e",
        "2001:4860:4860::8888",
        "::ffff:8.8.8.8", // mapped public IPv4 is judged as IPv4
        "2c0f:f248::1",
        "3ffe::1",
    ] {
        assert!(is_public(ip(public)), "{public} must be allowed");
    }
}

#[test]
fn check_ip_unmaps_ipv4() {
    let p = policy();
    assert_eq!(p.check_ip(ip("::ffff:8.8.8.8")), Ok(ip("8.8.8.8")));
    assert_eq!(
        p.check_ip(ip("::ffff:127.0.0.1")),
        Err(SubscriptionError::BlockedAddress)
    );
    assert_eq!(
        p.check_ip(ip("127.0.0.1")),
        Err(SubscriptionError::BlockedAddress)
    );
}

#[test]
fn urls_are_normalised() {
    let p = policy();
    let url = p
        .parse_url("webcal://Calendar.Example.COM/feed.ics?token=s3cret")
        .unwrap();
    assert!(url.https);
    assert_eq!(url.host, Host::Name("calendar.example.com".to_owned()));
    assert_eq!(url.port, 443);
    assert_eq!(url.path_and_query, "/feed.ics?token=s3cret");
    assert_eq!(
        url.as_string(),
        "https://calendar.example.com/feed.ics?token=s3cret"
    );
    // Debug output never shows the path or query.
    let debug = format!("{url:?}");
    assert!(
        !debug.contains("s3cret") && !debug.contains("feed.ics"),
        "{debug}"
    );

    let url = p.parse_url("  WEBCALS://example.org.  ").unwrap();
    assert_eq!(url.as_string(), "https://example.org/");
    let url = p.parse_url("https://example.org?x=1#fragment").unwrap();
    assert_eq!(url.path_and_query, "/?x=1");
    let url = p.parse_url("https://[2606:4700:4700::1111]/cal").unwrap();
    assert_eq!(url.host_header(), "[2606:4700:4700::1111]");
    assert_eq!(url.server_name(), "2606:4700:4700::1111");
    let url = p.parse_url("https://8.8.8.8/cal").unwrap();
    assert_eq!(url.host, Host::Ip(ip("8.8.8.8")));
    assert_eq!(p.parse_url("https://example.org:443/").unwrap().port, 443);
}

#[test]
fn http_only_when_allowed() {
    let err = policy()
        .parse_url("http://example.org/cal.ics")
        .unwrap_err();
    assert_eq!(err.code(), "invalid_url");
    let url = http_policy()
        .parse_url("http://example.org/cal.ics")
        .unwrap();
    assert!(!url.https);
    assert_eq!(url.port, 80);
    // Port 80 is only allowed together with http.
    assert!(policy().parse_url("https://example.org:80/").is_err());
    assert!(http_policy().parse_url("https://example.org:80/").is_ok());
}

#[test]
fn bad_urls_are_refused() {
    let p = policy();
    let long = format!("https://example.org/{}", "a".repeat(MAX_URL_LEN));
    for bad in [
        "",
        "example.org/cal.ics",
        "ftp://example.org/cal.ics",
        "file:///etc/passwd",
        "gopher://example.org:70/",
        "javascript:alert(1)",
        "https://user:pass@example.org/cal.ics",
        "https://user@example.org/",
        "https://example.org@evil.example/",
        "https://example.org:8443/cal.ics",
        "https://example.org:0/",
        "https://example.org:65536/",
        "https://example.org:/",
        "https://example.org:443abc/",
        "https://exa mple.org/",
        "https://example.org/a b",
        "https://example.org/\u{7f}",
        "https://example.org/\r\nHost: evil",
        "https://ex%61mple.org/",
        "https://bücher.example/",
        "https://[::1/",
        "https://[not-an-ip]/",
        "https://example/",
        "https://intranet/",
        "https://-bad.example.org/",
        "https://bad-.example.org/",
        "https://a..b.example/",
        "https://example.org\\@evil.example/",
        "https://example.org/é",
        long.as_str(),
    ] {
        let err = p.parse_url(bad).unwrap_err();
        assert!(
            matches!(
                err,
                SubscriptionError::InvalidUrl(_) | SubscriptionError::BlockedAddress
            ),
            "{bad}: {err:?}"
        );
    }
    // Internationalised names work in their xn-- form.
    assert!(p.parse_url("https://xn--bcher-kva.example/").is_ok());
}

#[test]
fn numeric_and_local_hosts_are_refused() {
    let p = policy();
    for bad in [
        // Address literals in private ranges.
        "https://127.0.0.1/",
        "https://10.1.2.3/",
        "https://169.254.169.254/latest/meta-data/",
        "https://0.0.0.0/",
        "https://255.255.255.255/",
        "https://[::1]/",
        "https://[::ffff:127.0.0.1]/",
        "https://[::ffff:7f00:1]/",
        "https://[fd00:ec2::254]/",
        "https://[fe80::1]/",
        "https://[64:ff9b::a00:1]/",
        // Decimal, octal and hex notations that inet_aton would accept.
        "https://2130706433/",
        "https://017700000001/",
        "https://0x7f000001/",
        "https://0x7f.0.0.1/",
        "https://0177.0.0.1/",
        "https://127.1/",
        "https://127.0.1/",
        "https://127.0.0.0x1/",
        "https://10.0.0.010/",
        "https://1.2.3.04/",
        // Trailing dots and local names.
        "https://127.0.0.1./",
        "https://localhost/",
        "https://localhost./",
        "https://LOCALHOST/",
        "https://api.localhost/",
        "https://printer.local/",
        "https://db.internal/",
        "https://router.home.arpa/",
        "https://1.0.0.127.in-addr.arpa/",
        "https://host.localdomain/",
    ] {
        let err = p.parse_url(bad).unwrap_err();
        assert!(
            matches!(
                err,
                SubscriptionError::InvalidUrl(_) | SubscriptionError::BlockedAddress
            ),
            "{bad}: {err:?}"
        );
    }
}

#[test]
fn host_lists() {
    let p = Policy::new(&SubscriptionsConfig {
        allowed_hosts: vec![
            "calendar.example.com".to_owned(),
            "*.feeds.example".to_owned(),
        ],
        denied_hosts: vec!["bad.feeds.example".to_owned()],
        ..SubscriptionsConfig::default()
    });
    assert!(p.parse_url("https://calendar.example.com/x").is_ok());
    assert!(p.parse_url("https://CALENDAR.example.com./x").is_ok());
    assert!(p.parse_url("https://a.feeds.example/x").is_ok());
    for refused in [
        "https://feeds.example/x",
        "https://bad.feeds.example/x",
        "https://other.example.com/x",
        "https://calendar.example.com.evil.example/x",
        "https://8.8.8.8/x",
    ] {
        assert_eq!(
            p.parse_url(refused),
            Err(SubscriptionError::BlockedAddress),
            "{refused}"
        );
    }
    let denied = Policy::new(&SubscriptionsConfig {
        denied_hosts: vec![".example.net".to_owned()],
        ..SubscriptionsConfig::default()
    });
    assert!(denied.parse_url("https://example.net/").is_ok());
    assert!(denied.parse_url("https://www.example.net/").is_err());
}

#[test]
fn redirect_targets_are_checked_again() {
    let p = policy();
    let base = p
        .parse_url("https://example.org/feeds/cal.ics?t=1")
        .unwrap();
    let go = |location: &str| p.redirect(&base, location).map(|u| u.as_string());
    assert_eq!(
        go("https://other.example/x.ics").unwrap(),
        "https://other.example/x.ics"
    );
    assert_eq!(
        go("webcal://other.example/x").unwrap(),
        "https://other.example/x"
    );
    assert_eq!(go("//cdn.example/x").unwrap(), "https://cdn.example/x");
    assert_eq!(go("/abs?y=2").unwrap(), "https://example.org/abs?y=2");
    assert_eq!(go("rel.ics").unwrap(), "https://example.org/feeds/rel.ics");
    assert_eq!(go("?t=2").unwrap(), "https://example.org/feeds/cal.ics?t=2");
    for refused in [
        "http://example.org/cal.ics",
        "https://127.0.0.1/",
        "https://[::1]/",
        "https://localhost/",
        "//169.254.169.254/latest",
        "https://example.org:8080/",
        "https://user@example.org/",
        "file:///etc/passwd",
        "gopher://example.org/",
        "",
    ] {
        assert!(go(refused).is_err(), "{refused}");
    }
    // https -> http is a downgrade even where http is allowed.
    let hp = http_policy();
    let base = hp.parse_url("https://example.org/").unwrap();
    assert_eq!(
        hp.redirect(&base, "http://example.org/"),
        Err(SubscriptionError::FetchFailed(Some(
            "redirect from https to http"
        )))
    );
    let plain = hp.parse_url("http://example.org/").unwrap();
    assert!(hp.redirect(&plain, "https://example.org/").is_ok());
}
