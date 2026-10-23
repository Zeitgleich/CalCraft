use super::*;

#[test]
fn colors() {
    assert!(is_valid_color("#1a2B3c"));
    assert!(is_valid_color("#1a2b3cff"));
    for bad in [
        "",
        "#",
        "1a2b3c",
        "#1a2b3",
        "#1a2b3cf",
        "#gggggg",
        "red",
        "#1a2b3c;x",
    ] {
        assert!(!is_valid_color(bad), "{bad:?}");
    }
    assert_eq!(
        color_for_picker(Some("#AABBCCDD")).as_deref(),
        Some("#aabbcc")
    );
    assert_eq!(color_for_picker(Some("blue")), None);
    assert_eq!(color_for_picker(None), None);
}

#[test]
fn feed_urls() {
    for good in [
        "https://example.org/cal.ics",
        "webcal://example.org/cal.ics?token=x",
        "WEBCAL://example.org",
        "http://example.org/feed",
    ] {
        assert!(check_feed_url(good).is_ok(), "{good}");
    }
    let long = format!("https://example.org/{}", "a".repeat(MAX_FEED_URL));
    for bad in [
        "",
        "https://",
        "example.org/cal.ics",
        "ftp://example.org/cal.ics",
        "javascript:alert(1)",
        "https://exa mple.org/",
        long.as_str(),
    ] {
        assert!(check_feed_url(bad).is_err(), "{bad}");
    }
}

#[test]
fn slugs_and_usernames() {
    assert!(check_slug("work-2026").is_ok());
    assert!(check_slug("Work").is_err());
    assert!(check_slug("../x").is_err());
    assert!(check_slug("").is_err());
    assert!(check_username("alice").is_ok());
    assert!(check_username("Alice Smith").is_err());
    assert!(check_group_name("team.a").is_ok());
    assert!(check_group_name("Team A").is_err());
}

#[test]
fn passwords() {
    assert_eq!(
        check_new_password("short", "short"),
        Err(text::ERR_PASSWORD_TOO_SHORT)
    );
    assert_eq!(
        check_new_password("ccap_0123456789", "ccap_0123456789"),
        Err(text::ERR_PASSWORD_RESERVED)
    );
    assert_eq!(
        check_new_password("long enough pw", "long enough px"),
        Err(text::ERR_PASSWORD_MISMATCH)
    );
    assert!(check_new_password("long enough pw", "long enough pw").is_ok());
    // Length counts characters, like the server.
    assert!(check_new_password("ääääääääääää", "ääääääääääää").is_ok());
}

#[test]
fn app_password_names() {
    assert!(check_app_password_name("Phone").is_ok());
    assert!(check_app_password_name("   ").is_err());
    assert!(check_app_password_name(&"x".repeat(65)).is_err());
}

#[test]
fn codes_and_lengths() {
    assert_eq!(clean_code(" 123 456 "), "123456");
    assert_eq!(clean_code("abcd-efgh"), "abcd-efgh");
    assert!(check_length("abc", 3).is_ok());
    assert!(check_length("abcd", 3).is_err());
}

#[test]
fn slug_suggestions() {
    assert_eq!(suggest_slug("Work Calendar"), "work-calendar");
    assert_eq!(suggest_slug("  Müll & Abfuhr!! "), "muell-abfuhr");
    assert_eq!(suggest_slug("***"), "");
    assert_eq!(suggest_slug(&"a".repeat(100)).len(), Slug::MAX_LEN);
    assert!(check_slug(&suggest_slug("Family 2026")).is_ok());
}
