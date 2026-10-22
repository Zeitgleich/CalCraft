use super::*;

#[test]
fn text_and_colors() {
    assert_eq!(clean_text("  Work ", 10).as_deref(), Some("Work"));
    assert!(clean_text("a\u{202E}b", 10).is_none());
    assert!(clean_text("a\nb", 10).is_none());
    assert!(clean_text(&"x".repeat(11), 10).is_none());
    assert_eq!(clean_color("#336699").as_deref(), Some("#336699"));
    assert_eq!(clean_color("#33669980").as_deref(), Some("#33669980"));
    for bad in [
        "336699",
        "#3366",
        "#33669G",
        "javascript:alert(1)",
        "#336699;",
    ] {
        assert!(clean_color(bad).is_none(), "{bad}");
    }
}
