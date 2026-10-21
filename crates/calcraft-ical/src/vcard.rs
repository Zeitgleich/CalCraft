//! Validation of CardDAV address object resources (RFC 6352 §5.1).
//!
//! vCard 3.0 (RFC 2426) and 4.0 (RFC 6350) share the content-line format of
//! iCalendar, so the same parser and limits apply.

use crate::{Component, IcalError, Property, parse};

const MAX_UID_LEN: usize = 1024;

/// A validated address object resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VCard {
    pub uid: String,
    /// `3.0` or `4.0`.
    pub version: &'static str,
}

fn single<'a>(card: &'a Component, name: &'a str) -> Result<Option<&'a Property>, IcalError> {
    let mut found = card.properties_named(name);
    let first = found.next();
    if found.next().is_some() {
        return Err(IcalError::Invalid("property must not occur more than once"));
    }
    Ok(first)
}

impl VCard {
    /// Parses and validates exactly one vCard as stored via PUT.
    pub fn parse(data: &[u8]) -> Result<Self, IcalError> {
        let card = parse(data)?;
        if card.name != "VCARD" {
            return Err(IcalError::Invalid("top-level component must be VCARD"));
        }
        if !card.components.is_empty() {
            return Err(IcalError::Invalid(
                "nested components are not allowed in a vCard",
            ));
        }
        let version = match single(&card, "VERSION")?.map(|p| p.value.as_str()) {
            Some("3.0") => "3.0",
            Some("4.0") => "4.0",
            _ => return Err(IcalError::Invalid("VERSION 3.0 or 4.0 is required")),
        };
        if card
            .property("FN")
            .is_none_or(|p| p.value.trim().is_empty())
        {
            return Err(IcalError::Invalid("FN is required"));
        }
        // CardDAV requires a UID so that clients can match resources.
        let uid = single(&card, "UID")?
            .map(|p| p.value.as_str())
            .ok_or(IcalError::Invalid("UID is required"))?;
        if uid.is_empty() || uid.len() > MAX_UID_LEN {
            return Err(IcalError::Invalid("invalid UID"));
        }
        Ok(Self {
            uid: uid.to_owned(),
            version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(body: &str) -> Result<VCard, IcalError> {
        VCard::parse(format!("BEGIN:VCARD\r\n{body}END:VCARD\r\n").as_bytes())
    }

    #[test]
    fn accepts_v3_and_v4() {
        let v4 = card(
            "VERSION:4.0\r\nUID:urn:uuid:1\r\nFN:Alice Example\r\nEMAIL:alice@example.com\r\n",
        )
        .unwrap();
        assert_eq!(v4.uid, "urn:uuid:1");
        assert_eq!(v4.version, "4.0");
        let v3 = card("VERSION:3.0\r\nUID:2\r\nFN:Bob\r\nN:Bob;;;;\r\nTEL;TYPE=CELL:+49 123\r\n")
            .unwrap();
        assert_eq!(v3.version, "3.0");
    }

    #[test]
    fn rejects_invalid_cards() {
        for body in [
            "UID:1\r\nFN:x\r\n",
            "VERSION:2.1\r\nUID:1\r\nFN:x\r\n",
            "VERSION:4.0\r\nVERSION:3.0\r\nUID:1\r\nFN:x\r\n",
            "VERSION:4.0\r\nFN:x\r\n",
            "VERSION:4.0\r\nUID:1\r\n",
            "VERSION:4.0\r\nUID:1\r\nFN: \r\n",
            "VERSION:4.0\r\nUID:1\r\nUID:2\r\nFN:x\r\n",
            "VERSION:4.0\r\nUID:1\r\nFN:x\r\nBEGIN:VCARD\r\nEND:VCARD\r\n",
        ] {
            assert!(card(body).is_err(), "{body:?}");
        }
        assert!(VCard::parse(b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n").is_err());
        assert!(VCard::parse(b"BEGIN:VCARD\r\nVERSION:4.0\r\nUID:1\r\nFN:x\r\nEND:VCARD\r\nBEGIN:VCARD\r\nEND:VCARD\r\n").is_err());
    }
}
