use super::*;

fn v(major: u64, minor: u64, patch: u64) -> Version {
    Version {
        major,
        minor,
        patch,
    }
}

#[test]
fn versions_parse_strictly() {
    assert_eq!(Version::from_tag("v1.2.3"), Some(v(1, 2, 3)));
    assert_eq!(Version::from_tag("v0.10.0"), Some(v(0, 10, 0)));
    assert_eq!(
        Version::from_tag("v18446744073709551615.0.0"),
        Some(v(u64::MAX, 0, 0))
    );
    for bad in [
        "1.2.3",
        "v1.2",
        "v1.2.3.4",
        "v1.2.3-rc1",
        "v1.2.3+build",
        "v01.2.3",
        "v1..3",
        "v1.2.",
        "v+1.2.3",
        "v-1.2.3",
        "v 1.2.3",
        "v1.2.3 ",
        "V1.2.3",
        "v18446744073709551616.0.0",
        "v١.2.3",
        "",
        "v",
    ] {
        assert_eq!(Version::from_tag(bad), None, "{bad}");
    }
}

#[test]
fn versions_compare_numerically() {
    assert!(v(0, 10, 0) > v(0, 9, 9));
    assert!(v(1, 0, 0) > v(0, 99, 99));
    assert!(v(1, 2, 10) > v(1, 2, 9));
    assert!(is_newer(v(0, 2, 0), "0.1.0"));
    assert!(is_newer(v(0, 1, 1), "0.1.0"));
    assert!(!is_newer(v(0, 1, 0), "0.1.0"));
    assert!(!is_newer(v(0, 0, 9), "0.1.0"));
    // A pre-release is older than its release, build metadata is not.
    assert!(is_newer(v(1, 0, 0), "1.0.0-rc.1"));
    assert!(!is_newer(v(1, 0, 0), "1.0.0+local"));
    assert!(!is_newer(v(0, 9, 0), "1.0.0-rc.1"));
    assert!(!is_newer(v(9, 9, 9), "garbage"));
}

#[test]
fn only_release_pages_of_this_repository_are_links() {
    for ok in [
        "https://github.com/Zeitgleich/CalCraft/releases/tag/v1.2.3",
        "https://github.com/Zeitgleich/CalCraft/releases/latest",
    ] {
        assert_eq!(release_link(ok), Some(ok));
    }
    for bad in [
        "http://github.com/Zeitgleich/CalCraft/releases/tag/v1.2.3",
        "https://github.com/Zeitgleich/CalCraft/releasesx",
        "https://github.com/Zeitgleich/CalCraft/releases",
        "https://github.com/Other/CalCraft/releases/tag/v1.2.3",
        "https://github.com.evil.example/Zeitgleich/CalCraft/releases/x",
        "https://github.com/Zeitgleich/CalCraft/releases/../../../evil",
        "https://github.com/Zeitgleich/CalCraft/releases/tag/v1?x=1",
        "https://github.com/Zeitgleich/CalCraft/releases/tag/v1#x",
        "https://github.com/Zeitgleich/CalCraft/releases/tag/\"onclick",
        "https://github.com/Zeitgleich/CalCraft/releases/tag/v1 x",
        "javascript:alert(1)//https://github.com/Zeitgleich/CalCraft/releases/",
        "",
    ] {
        assert_eq!(release_link(bad), None, "{bad}");
    }
    let long = format!("{RELEASE_PAGE_PREFIX}{}", "a".repeat(MAX_URL_LEN));
    assert_eq!(release_link(&long), None);
}

fn release_json(tag: &str, url: &str, prerelease: bool, draft: bool) -> Vec<u8> {
    serde_json::json!({
        "tag_name": tag,
        "html_url": url,
        "prerelease": prerelease,
        "draft": draft,
        "name": "ignored",
        "assets": [{"name": "calcraft.tar.gz"}],
        "body": "Release notes"
    })
    .to_string()
    .into_bytes()
}

const PAGE: &str = "https://github.com/Zeitgleich/CalCraft/releases/tag/v9.0.0";

#[test]
fn release_json_is_parsed_defensively() {
    let release = parse_release(&release_json("v9.0.0", PAGE, false, false))
        .unwrap()
        .unwrap();
    assert_eq!(release.version, v(9, 0, 0));
    assert_eq!(release.url.as_deref(), Some(PAGE));

    // Foreign links are dropped, the version is kept.
    let foreign = parse_release(&release_json(
        "v9.0.0",
        "https://evil.example/",
        false,
        false,
    ))
    .unwrap()
    .unwrap();
    assert_eq!(foreign.url, None);

    // Drafts, pre-releases and odd tags are ignored.
    for body in [
        release_json("v9.0.0", PAGE, true, false),
        release_json("v9.0.0", PAGE, false, true),
        release_json("v9.0.0-rc1", PAGE, false, false),
        release_json("nightly", PAGE, false, false),
        release_json(&format!("v{}", "9".repeat(80)), PAGE, false, false),
        br#"{"html_url": "https://github.com/Zeitgleich/CalCraft/releases/x"}"#.to_vec(),
        b"{}".to_vec(),
    ] {
        assert_eq!(parse_release(&body), Ok(None));
    }

    // Malformed input is an error whose message does not quote it.
    for body in [
        b"".to_vec(),
        b"not json".to_vec(),
        b"[1, 2]".to_vec(),
        br#"{"tag_name": 5}"#.to_vec(),
        br#"{"tag_name": "v1.0.0", "draft": "zqsecret"}"#.to_vec(),
        br#"{"tag_name": "v1.0.0""#.to_vec(),
        vec![0xff, 0xfe, 0x00],
    ] {
        let err = parse_release(&body).unwrap_err();
        assert!(matches!(err, ReleaseError::Malformed { .. }));
        assert!(!err.to_string().contains("zqsecret"));
    }

    // Oversized bodies are refused before parsing.
    let mut huge = release_json("v9.0.0", PAGE, false, false);
    huge.truncate(huge.len() - 1);
    huge.extend_from_slice(b",\"padding\":\"");
    huge.resize(MAX_RESPONSE + 1, b'x');
    assert_eq!(parse_release(&huge), Err(ReleaseError::TooLarge));
}

#[test]
fn state_reports_newer_releases_and_keeps_results_on_errors() {
    let check = UpdateCheck::new();
    let info = check.info();
    assert!(info.enabled && !info.newer);
    assert_eq!(info.current, calcraft_core::VERSION);
    assert_eq!((info.latest, info.url, info.checked_at), (None, None, None));

    check
        .apply(&release_json("v9999.0.0", PAGE, false, false), 100)
        .unwrap();
    let info = check.info();
    assert!(info.newer);
    assert_eq!(info.latest.as_deref(), Some("9999.0.0"));
    assert_eq!(info.url.as_deref(), Some(PAGE));
    assert_eq!(info.checked_at, Some(100));

    assert!(check.apply(b"garbage", 200).is_err());
    assert_eq!(check.info().checked_at, Some(100));

    check
        .apply(&release_json("v0.0.1", PAGE, false, false), 300)
        .unwrap();
    let info = check.info();
    assert!(!info.newer);
    assert_eq!(info.latest.as_deref(), Some("0.0.1"));
}

#[test]
fn jitter_is_bounded() {
    let interval = Duration::from_secs(3600);
    for _ in 0..100 {
        let delay = with_jitter(interval);
        assert!(delay >= interval && delay <= interval + Duration::from_secs(360));
    }
    assert_eq!(with_jitter(Duration::ZERO), Duration::ZERO);
}
