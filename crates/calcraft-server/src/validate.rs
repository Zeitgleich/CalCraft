//! Validation of user-supplied names, descriptions and colors, shared by
//! DAV and the REST API so both enforce identical rules.

pub const MAX_DISPLAY_NAME: usize = 256;
pub const MAX_DESCRIPTION: usize = 4096;

/// Invisible formatting characters that can disguise names: zero-width
/// characters, bidirectional overrides and isolates, byte order mark.
fn is_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}'
    )
}

/// Trims and validates free text; `None` if too long or containing
/// control or invisible formatting characters.
pub fn clean_text(text: &str, max: usize) -> Option<String> {
    let text = text.trim();
    (text.chars().count() <= max && !text.chars().any(|c| c.is_control() || is_format_char(c)))
        .then(|| text.to_owned())
}

/// `#RRGGBB` or `#RRGGBBAA`, as used by Apple and Thunderbird.
pub fn clean_color(text: &str) -> Option<String> {
    let text = text.trim();
    let hex = text.strip_prefix('#')?;
    (matches!(hex.len(), 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| text.to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
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
}
