//! Validation of CardDAV address object resources (RFC 6352 §5.1).
//!
//! vCard 3.0 (RFC 2426) and 4.0 (RFC 6350) share the content-line format of
//! iCalendar, so the same parser and limits apply, except for a larger
//! size limit ([`crate::MAX_VCARD_SIZE`]) because of embedded photos.
//!
//! Photos and logos are checked without ever looking at the image bytes:
//! embedded data must be syntactically valid base64 of an allowed image
//! type, references must be `http`/`https` URIs. The server stores such
//! URIs but never fetches them.
//!
//! Groups (vCard 4.0 `KIND:group` with `MEMBER`, Apple's vCard 3.0
//! `X-ADDRESSBOOKSERVER-KIND`/`-MEMBER`) are ordinary vCards with a UID
//! and are stored unchanged.

use crate::{Component, IcalError, Property, parser::parse_vcard};

const MAX_UID_LEN: usize = 1024;

/// Image types accepted for embedded `PHOTO`/`LOGO` data, as the subtype
/// of `image/…` or as a bare vCard 3.0 `TYPE` (`JPEG`).
const IMAGE_TYPES: [&str; 5] = ["jpeg", "jpg", "png", "gif", "webp"];

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

fn is_allowed_image_type(media_type: &str) -> bool {
    let media_type = media_type.trim().to_ascii_lowercase();
    let subtype = media_type.strip_prefix("image/").unwrap_or(&media_type);
    IMAGE_TYPES.contains(&subtype)
}

/// Standard base64 (RFC 4648 §4), padding optional, no whitespace.
fn is_base64(data: &str) -> bool {
    let body = data.trim_end_matches('=');
    let padding = data.len() - body.len();
    !body.is_empty()
        && padding <= 2
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
        && match body.len() % 4 {
            0 => padding == 0,
            1 => false,
            rest => padding == 0 || rest + padding == 4,
        }
}

/// Checks a `data:` URI (RFC 2397): an allowed image type, base64.
fn check_data_uri(uri: &str) -> Result<(), IcalError> {
    let invalid = IcalError::Invalid("unsupported embedded image");
    let (header, data) = uri.split_once(',').ok_or(invalid.clone())?;
    // Some writers escape `;` and `,` like TEXT; only the header can contain them.
    let header = header
        .strip_suffix('\\')
        .unwrap_or(header)
        .replace("\\;", ";");
    let mut parts = header.split(';');
    let media_type = parts.next().unwrap_or_default();
    let mut base64 = false;
    for part in parts {
        if part.eq_ignore_ascii_case("base64") {
            base64 = true;
        } else if !part.contains('=') {
            return Err(invalid);
        }
    }
    let is_image = media_type
        .get(..6)
        .is_some_and(|p| p.eq_ignore_ascii_case("image/"));
    if !base64
        || !is_image
        || !is_allowed_image_type(media_type)
        || !is_base64(data)
        || !has_image_magic(data)
    {
        return Err(invalid);
    }
    Ok(())
}

/// Decodes the first bytes of valid base64, enough for a magic number.
fn base64_prefix(data: &str) -> Vec<u8> {
    let sextet = |b: u8| match b {
        b'A'..=b'Z' => b - b'A',
        b'a'..=b'z' => b - b'a' + 26,
        b'0'..=b'9' => b - b'0' + 52,
        b'+' => 62,
        _ => 63,
    };
    let chars: Vec<u8> = data.bytes().take(16).take_while(|&b| b != b'=').collect();
    let mut out = Vec::new();
    for chunk in chars.chunks(4) {
        let bits = chunk.iter().enumerate().fold(0u32, |acc, (i, &b)| {
            acc | u32::from(sextet(b)) << (18 - 6 * i)
        });
        let bytes = bits.to_be_bytes();
        out.extend_from_slice(&bytes[1..chunk.len()]);
    }
    out
}

/// Whether the data starts like a JPEG, PNG, GIF or WebP file. Only the
/// magic number is looked at, never the image itself.
fn has_image_magic(data: &str) -> bool {
    let head = base64_prefix(data);
    head.starts_with(&[0xFF, 0xD8, 0xFF])
        || head.starts_with(b"\x89PNG\r\n\x1a\n")
        || head.starts_with(b"GIF87a")
        || head.starts_with(b"GIF89a")
        || (head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP"))
}

/// All values of a parameter, over repeated occurrences and comma lists.
fn parameter_values<'a>(property: &'a Property, name: &str) -> Vec<&'a str> {
    property
        .parameters
        .iter()
        .filter(|p| p.name == name)
        .flat_map(|p| p.values.iter())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .collect()
}

/// Checks a `PHOTO` or `LOGO` property (RFC 6350 §6.2.4, RFC 2426 §3.1.4).
fn check_image(property: &Property) -> Result<(), IcalError> {
    let invalid = IcalError::Invalid("unsupported embedded image");
    let value = property.value.as_str();
    let encodings = parameter_values(property, "ENCODING");
    // Every ENCODING must mean base64 (`b` and `BASE64` are synonyms).
    if !encodings
        .iter()
        .all(|e| e.eq_ignore_ascii_case("b") || e.eq_ignore_ascii_case("base64"))
    {
        return Err(invalid);
    }
    let embedded = !encodings.is_empty();
    // VALUE must be unambiguous and fit the encoding: `binary` for inline
    // data, `uri` otherwise.
    let expected = if embedded { "binary" } else { "uri" };
    if !parameter_values(property, "VALUE")
        .iter()
        .all(|v| v.eq_ignore_ascii_case(expected))
    {
        return Err(IcalError::Invalid("conflicting VALUE parameter"));
    }
    // Every declared TYPE counts: clients may read any of them.
    if !parameter_values(property, "TYPE")
        .iter()
        .all(|t| is_allowed_image_type(t))
    {
        return Err(invalid);
    }
    // Exports (iOS, Android, Synology) contain empty photos; harmless.
    if value.is_empty() {
        return Ok(());
    }
    if embedded {
        // vCard 3.0 inline binary: `ENCODING=b;TYPE=JPEG:<base64>`. The magic
        // number must show an allowed image type, declared or not.
        if !is_base64(value) || !has_image_magic(value) {
            return Err(invalid);
        }
        return Ok(());
    }
    // The prefixes are ASCII, so the slices below are on char boundaries.
    let strip = |prefix: &str| {
        value
            .as_bytes()
            .get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix.as_bytes()))
            .then(|| &value[prefix.len()..])
    };
    if let Some(uri) = strip("data:") {
        return check_data_uri(uri);
    }
    // Stored only; the server never dereferences them.
    let Some(rest) = strip("https://").or_else(|| strip("http://")) else {
        return Err(IcalError::Invalid(
            "photo URI must be data:, http: or https:",
        ));
    };
    if rest.is_empty() || rest.contains(char::is_whitespace) {
        return Err(IcalError::Invalid("invalid photo URI"));
    }
    Ok(())
}

impl VCard {
    /// Parses and validates exactly one vCard as stored via PUT.
    pub fn parse(data: &[u8]) -> Result<Self, IcalError> {
        let card = parse_vcard(data)?;
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
        for property in card
            .properties
            .iter()
            .filter(|p| matches!(p.name.as_str(), "PHOTO" | "LOGO"))
        {
            check_image(property)?;
        }
        // RFC 6350 §6.1.4, §6.6.5: at most one KIND, MEMBER only in groups.
        let group = single(&card, "KIND")?.is_some_and(|k| k.value.eq_ignore_ascii_case("group"));
        if version == "4.0" && !group && card.property("MEMBER").is_some() {
            return Err(IcalError::Invalid("MEMBER requires KIND:group"));
        }
        Ok(Self {
            uid: uid.to_owned(),
            version,
        })
    }
}

#[cfg(test)]
mod tests;
