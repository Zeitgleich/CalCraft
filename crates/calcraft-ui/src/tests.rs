use super::*;

#[test]
fn parses_error_bodies() {
    let failure =
        ApiFailure::from_response(401, r#"{"error":"totp_required","message":"code needed"}"#);
    assert!(failure.is_totp_required());
    assert_eq!(failure.user_message(), text::ERR_TOTP_REQUIRED);

    let conflict = ApiFailure::from_response(
        409,
        r#"{"error":"conflict","message":"user already exists"}"#,
    );
    assert_eq!(conflict.user_message(), "User already exists");

    let garbage = ApiFailure::from_response(502, "<html>bad gateway</html>");
    assert_eq!(garbage.code, "unexpected");
    assert!(garbage.user_message().contains("502"));
}

#[test]
fn unauthorized_is_detected() {
    let failure = ApiFailure::from_response(
        401,
        r#"{"error":"unauthorized","message":"login required"}"#,
    );
    assert!(failure.is_unauthorized());
    assert!(!ApiFailure::network("offline").is_unauthorized());
}

#[test]
fn formats_timestamps() {
    assert_eq!(format_timestamp(0), "1970-01-01 00:00 UTC");
    assert_eq!(format_timestamp(951_782_400), "2000-02-29 00:00 UTC");
    assert_eq!(format_timestamp(1_792_581_240), "2026-10-21 11:14 UTC");
    assert_eq!(format_timestamp(-60), "1969-12-31 23:59 UTC");
}

#[test]
fn formats_sizes() {
    assert_eq!(format_size(0), "0 B");
    assert_eq!(format_size(999), "999 B");
    assert_eq!(format_size(1000), "1.0 kB");
    assert_eq!(format_size(1549), "1.5 kB");
    assert_eq!(format_size(999_949), "999.9 kB");
    assert_eq!(format_size(2_500_000), "2.5 MB");
    assert_eq!(format_size(-5), "0 B");
}

#[test]
fn restore_conflicts_have_texts() {
    for code in [
        "name_taken",
        "uid_taken",
        "collection_exists",
        "collection_in_trash",
        "collection_deleted",
        "invalid_target",
    ] {
        let body = format!(r#"{{"error":"{code}","message":"x"}}"#);
        let failure = ApiFailure::from_response(409, &body);
        assert_ne!(failure.user_message(), "X", "{code}");
    }
}

#[test]
fn groups_secrets() {
    assert_eq!(group_secret("ABCDEFGHIJ"), "ABCD EFGH IJ");
    assert_eq!(group_secret(""), "");
}

#[test]
fn pages_round_trip() {
    for page in Page::ALL {
        assert_eq!(Page::from_hash(page.hash()), page);
    }
    assert_eq!(Page::from_hash(""), Page::Overview);
    assert_eq!(Page::from_hash("#/nonsense"), Page::Overview);
    assert!(!Page::Admin.visible(false));
    assert!(Page::Admin.visible(true));
}
