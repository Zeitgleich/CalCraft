//! Managed attachments (RFC 8607): `ATTACH` properties with a `MANAGED-ID`
//! parameter in calendar objects, and the HTTP helpers the server needs to
//! accept and serve them (`Content-Disposition`, file names, media types).
//!
//! Only `ATTACH` properties of the instance components (`VEVENT`, `VTODO`,
//! `VJOURNAL`) count as managed attachments; alarms are left alone.

use std::collections::HashSet;

use crate::{
    Component, Parameter, Property,
    recurrence::{self, Budget, InstantKeys, OBJECT_BUDGET},
};

/// Parameter naming the server's identifier of a managed attachment.
pub const MANAGED_ID: &str = "MANAGED-ID";
/// Most entries of a `rid` list.
pub const MAX_RIDS: usize = 100;
/// Most instance components an object may end up with after an attachment
/// operation created overrides.
pub const MAX_INSTANCE_COMPONENTS: usize = crate::itip::MAX_INSTANCE_COMPONENTS;
/// Longest file name kept (bytes of UTF-8).
pub const MAX_FILENAME_LEN: usize = 255;
/// Longest accepted `Content-Disposition` header.
pub const MAX_DISPOSITION_LEN: usize = 2048;
/// File name used when a request has none (or nothing is left of it).
pub const DEFAULT_FILENAME: &str = "attachment";
/// Media type of attachments without a usable one.
pub const OCTET_STREAM: &str = "application/octet-stream";

/// One entry of a `rid` query parameter (RFC 8607 §3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rid {
    /// `M`: the master component.
    Master,
    /// A RECURRENCE-ID value as written in the object.
    Instance(String),
}

/// Why an attachment operation cannot be applied to an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachError {
    /// A `rid` names no instance of the object (`CALDAV:valid-rid`).
    InvalidRid,
    /// The managed id is not on the targeted components
    /// (`CALDAV:valid-managed-id`).
    UnknownManagedId,
    /// The object would get too many components.
    TooManyComponents,
}

fn is_instance(component: &Component) -> bool {
    matches!(component.name.as_str(), "VEVENT" | "VTODO" | "VJOURNAL")
}

/// The managed id of an `ATTACH` property, if it is a managed attachment.
pub fn managed_id(property: &Property) -> Option<&str> {
    if property.name != "ATTACH" {
        return None;
    }
    property
        .parameter(MANAGED_ID)
        .and_then(|p| p.values.first())
        .map(String::as_str)
}

/// The distinct managed ids referenced by the instance components, in
/// order of appearance.
pub fn managed_ids(calendar: &Component) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for component in calendar.components.iter().filter(|c| is_instance(c)) {
        for id in component.properties.iter().filter_map(managed_id) {
            if seen.insert(id) {
                ids.push(id.to_owned());
            }
        }
    }
    ids
}

/// A managed `ATTACH` property with the server's parameters (RFC 8607 §4).
pub fn managed_property(
    url: &str,
    managed_id: &str,
    filename: &str,
    media_type: &str,
    size: u64,
) -> Property {
    let parameter = |name: &str, value: &str| Parameter {
        name: name.to_owned(),
        values: vec![value.to_owned()],
    };
    Property {
        name: "ATTACH".to_owned(),
        parameters: vec![
            parameter("FMTTYPE", media_type),
            parameter("SIZE", &size.to_string()),
            parameter("FILENAME", filename),
            parameter(MANAGED_ID, managed_id),
        ],
        value: url.to_owned(),
    }
}

/// Parses a `rid` query value: comma-separated, `M` (any case) at most once,
/// no duplicates, at most [`MAX_RIDS`] entries.
pub fn parse_rids(value: &str) -> Result<Vec<Rid>, AttachError> {
    let mut rids = Vec::new();
    for part in value.split(',') {
        let rid = if part.eq_ignore_ascii_case("M") {
            Rid::Master
        } else if !part.is_empty()
            && part.len() <= 32
            && part
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'T' | b'Z'))
        {
            Rid::Instance(part.to_owned())
        } else {
            return Err(AttachError::InvalidRid);
        };
        if rids.contains(&rid) || rids.len() >= MAX_RIDS {
            return Err(AttachError::InvalidRid);
        }
        rids.push(rid);
    }
    Ok(rids)
}

/// Indices of the instance components selected by `rids`, creating
/// overrides from the master for instances that have none yet (only where
/// `needs_override(master)` holds; otherwise the rid is invalid).
fn select(
    calendar: &mut Component,
    rids: &[Rid],
    needs_override: &dyn Fn(&Component) -> bool,
) -> Result<Vec<usize>, AttachError> {
    let master = calendar
        .components
        .iter()
        .position(|c| is_instance(c) && c.property("RECURRENCE-ID").is_none());
    let mut selected = Vec::new();
    let mut additions: Vec<String> = Vec::new();
    {
        let root: &Component = calendar;
        let mut keys = InstantKeys::new(root);
        // Keys of the existing overrides.
        let existing: Vec<(usize, Option<recurrence::InstantKey>)> = root
            .components
            .iter()
            .enumerate()
            .filter(|(_, c)| is_instance(c))
            .filter_map(|(i, c)| {
                c.property("RECURRENCE-ID")
                    .map(|rid| (i, keys.key(rid, &rid.value)))
            })
            .collect();
        let start = master.and_then(|m| root.components[m].property("DTSTART"));
        let mut budget = Budget::new(OBJECT_BUDGET.saturating_mul(4));
        for rid in rids {
            let value = match rid {
                Rid::Master => {
                    selected.push(master.ok_or(AttachError::InvalidRid)?);
                    continue;
                }
                Rid::Instance(value) => value,
            };
            // The rid is written like the master's DTSTART: same TZID
            // (unless it is UTC), VALUE=DATE for dates.
            let mut parameters: Vec<Parameter> = start
                .map(|s| {
                    s.parameters
                        .iter()
                        .filter(|p| p.name == "TZID" && !value.ends_with('Z'))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if value.len() == 8 {
                parameters.push(Parameter {
                    name: "VALUE".to_owned(),
                    values: vec!["DATE".to_owned()],
                });
            }
            let property = Property {
                name: "RECURRENCE-ID".to_owned(),
                parameters,
                value: value.clone(),
            };
            let key = {
                let mut own = InstantKeys::new(root);
                // The synthetic property only lives here; keys of the
                // object's own values come from `keys`.
                own.key(&property, value).ok_or(AttachError::InvalidRid)?
            };
            if let Some((index, _)) = existing.iter().find(|(_, k)| k.as_ref() == Some(&key)) {
                selected.push(*index);
                continue;
            }
            let Some(master) = master else {
                return Err(AttachError::InvalidRid);
            };
            if !needs_override(&root.components[master]) {
                return Err(AttachError::UnknownManagedId);
            }
            match recurrence::master_instance(root, &key, &mut budget) {
                Ok(Some(value)) if !additions.contains(&value) => additions.push(value),
                _ => return Err(AttachError::InvalidRid),
            }
        }
    }
    let instances = calendar
        .components
        .iter()
        .filter(|c| is_instance(c))
        .count();
    if instances + additions.len() > MAX_INSTANCE_COMPONENTS {
        return Err(AttachError::TooManyComponents);
    }
    if let Some(master) = master {
        for value in additions {
            let component = crate::itip::instance_override(&calendar.components[master], &value);
            calendar.components.push(component);
            selected.push(calendar.components.len() - 1);
        }
    }
    selected.sort_unstable();
    selected.dedup();
    Ok(selected)
}

fn all_instances(calendar: &Component) -> Vec<usize> {
    calendar
        .components
        .iter()
        .enumerate()
        .filter(|(_, c)| is_instance(c))
        .map(|(i, _)| i)
        .collect()
}

/// Adds `attach` to the instances named by `rids`, or to every instance
/// component (master and overrides) without `rids` (RFC 8607 §3.4).
pub fn add(
    calendar: &mut Component,
    attach: &Property,
    rids: Option<&[Rid]>,
) -> Result<(), AttachError> {
    let targets = match rids {
        Some(rids) => select(calendar, rids, &|_| true)?,
        None => all_instances(calendar),
    };
    if targets.is_empty() {
        return Err(AttachError::InvalidRid);
    }
    for index in targets {
        calendar.components[index].properties.push(attach.clone());
    }
    Ok(())
}

fn has(component: &Component, id: &str) -> bool {
    component
        .properties
        .iter()
        .any(|p| managed_id(p) == Some(id))
}

/// Removes the managed attachment `id` from the instances named by `rids`,
/// or from every instance without `rids` (RFC 8607 §3.6). Fails if none of
/// the targeted components had it.
pub fn remove(calendar: &mut Component, id: &str, rids: Option<&[Rid]>) -> Result<(), AttachError> {
    let targets = match rids {
        Some(rids) => select(calendar, rids, &|master| has(master, id))?,
        None => all_instances(calendar),
    };
    let mut removed = false;
    for index in targets {
        let properties = &mut calendar.components[index].properties;
        let before = properties.len();
        properties.retain(|p| managed_id(p) != Some(id));
        removed |= properties.len() != before;
    }
    if removed {
        Ok(())
    } else {
        Err(AttachError::UnknownManagedId)
    }
}

/// Replaces every `ATTACH` with managed id `id` by `attach` (an update,
/// RFC 8607 §3.5, applies to all instances).
pub fn replace(calendar: &mut Component, id: &str, attach: &Property) -> Result<(), AttachError> {
    let mut replaced = false;
    for component in calendar.components.iter_mut().filter(|c| is_instance(c)) {
        for property in &mut component.properties {
            if managed_id(property) == Some(id) {
                *property = attach.clone();
                replaced = true;
            }
        }
    }
    if replaced {
        Ok(())
    } else {
        Err(AttachError::UnknownManagedId)
    }
}

/// Rewrites managed `ATTACH` properties to the server's canonical form
/// (`canonical(id)`), so that clients cannot change the URL, size or name
/// of an attachment. Properties `canonical` does not know stay as they are.
/// Returns whether anything changed.
pub fn canonicalize(
    calendar: &mut Component,
    canonical: &dyn Fn(&str) -> Option<Property>,
) -> bool {
    let mut changed = false;
    for component in calendar.components.iter_mut().filter(|c| is_instance(c)) {
        for property in &mut component.properties {
            if let Some(attach) = managed_id(property).and_then(canonical)
                && *property != attach
            {
                *property = attach;
                changed = true;
            }
        }
    }
    changed
}

/// Removes every managed `ATTACH` (e.g. from a cancelled attendee copy,
/// which must no longer grant access). Returns whether anything changed.
pub fn strip_managed(calendar: &mut Component) -> bool {
    let mut changed = false;
    for component in calendar.components.iter_mut().filter(|c| is_instance(c)) {
        let before = component.properties.len();
        component.properties.retain(|p| managed_id(p).is_none());
        changed |= component.properties.len() != before;
    }
    changed
}

// --- HTTP helpers --------------------------------------------------------

fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

fn is_attr_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b)
}

/// Splits `value` at `;` outside quoted strings.
fn split_params(value: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (i, c) in value.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ';' if !quoted => {
                parts.push(&value[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if quoted {
        return None;
    }
    parts.push(&value[start..]);
    Some(parts)
}

/// The value of a quoted string (RFC 9110 §5.6.4) or token.
fn param_value(raw: &str) -> Option<String> {
    let raw = raw.trim_matches([' ', '\t']);
    if let Some(inner) = raw.strip_prefix('"') {
        let inner = inner.strip_suffix('"')?;
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            out.push(if c == '\\' { chars.next()? } else { c });
        }
        Some(out)
    } else if !raw.is_empty() && raw.bytes().all(is_tchar) {
        Some(raw.to_owned())
    } else {
        None
    }
}

/// Decodes an RFC 8187 `ext-value` (`UTF-8''f%C3%B6o.txt`).
fn ext_value(raw: &str) -> Option<String> {
    let raw = raw.trim_matches([' ', '\t']);
    let (charset, rest) = raw.split_once('\'')?;
    let (_language, encoded) = rest.split_once('\'')?;
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = encoded.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b if is_attr_char(b) => {
                out.push(b);
                i += 1;
            }
            _ => return None,
        }
    }
    if charset.eq_ignore_ascii_case("UTF-8") {
        String::from_utf8(out).ok()
    } else if charset.eq_ignore_ascii_case("ISO-8859-1") {
        Some(out.into_iter().map(char::from).collect())
    } else {
        None
    }
}

/// The file name of a `Content-Disposition` request header (RFC 6266):
/// `filename*` wins over `filename`. `None` if the header is malformed or
/// names no file. The result still needs [`sanitize_filename`].
pub fn disposition_filename(header: &str) -> Option<String> {
    if header.len() > MAX_DISPOSITION_LEN {
        return None;
    }
    let parts = split_params(header)?;
    let (kind, params) = parts.split_first()?;
    let kind = kind.trim_matches([' ', '\t']);
    if kind.is_empty() || !kind.bytes().all(is_tchar) {
        return None;
    }
    let mut plain = None;
    let mut extended = None;
    for param in params {
        let Some((name, value)) = param.split_once('=') else {
            continue;
        };
        let name = name.trim_matches([' ', '\t']);
        if name.eq_ignore_ascii_case("filename*") {
            extended = extended.or_else(|| ext_value(value));
        } else if name.eq_ignore_ascii_case("filename") {
            plain = plain.or_else(|| param_value(value));
        }
    }
    extended.or(plain)
}

/// Characters that would let a file name be displayed differently from
/// what it is (bidirectional overrides, invisible characters).
fn is_deceptive(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2069}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
    )
}

/// A safe file name (RFC 6266 §4.3): the last path component only, without
/// control, bidirectional or invisible characters, quotes and backslashes,
/// leading dots or surrounding blanks, at most [`MAX_FILENAME_LEN`] bytes;
/// [`DEFAULT_FILENAME`] if nothing is left.
pub fn sanitize_filename(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut name = String::with_capacity(last.len().min(MAX_FILENAME_LEN));
    for c in last.chars() {
        let c = match c {
            '"' | ':' | '*' | '?' | '<' | '>' | '|' => '_',
            c if c.is_control() || is_deceptive(c) => continue,
            c if c.is_whitespace() => ' ',
            c => c,
        };
        if name.len() + c.len_utf8() > MAX_FILENAME_LEN {
            break;
        }
        name.push(c);
    }
    let trimmed = name.trim_matches(|c: char| c == ' ' || c == '.');
    if trimmed.is_empty() {
        DEFAULT_FILENAME.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `Content-Disposition` response value for a download: always
/// `attachment`, an ASCII fallback and the exact name as RFC 8187
/// `filename*`. Only visible ASCII, so it can never inject header lines.
pub fn disposition_header(filename: &str) -> String {
    let fallback: String = filename
        .chars()
        .map(|c| {
            if (c.is_ascii_graphic() && c != '"' && c != '\\') || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::with_capacity(filename.len() * 3);
    for b in filename.bytes() {
        if is_attr_char(b) {
            encoded.push(char::from(b));
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

/// A media type `type/subtype` (lower case, no parameters) from a
/// `Content-Type` header, or `None` if it is not one.
pub fn media_type(header: &str) -> Option<String> {
    let essence = header.split(';').next()?.trim_matches([' ', '\t']);
    let (kind, subtype) = essence.split_once('/')?;
    let valid = |part: &str| !part.is_empty() && part.len() <= 63 && part.bytes().all(is_tchar);
    (valid(kind) && valid(subtype)).then(|| essence.to_ascii_lowercase())
}

/// Media types the server sends as `Content-Type` of a download; anything
/// else (HTML, SVG, XML, scripts, unknown types) goes out as
/// [`OCTET_STREAM`], so that nothing is rendered on the server's origin.
const SAFE_MEDIA_TYPES: &[&str] = &[
    "application/pdf",
    "application/zip",
    "application/msword",
    "application/vnd.ms-excel",
    "application/vnd.ms-powerpoint",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/vnd.oasis.opendocument.text",
    "application/vnd.oasis.opendocument.spreadsheet",
    "application/vnd.oasis.opendocument.presentation",
    "audio/mpeg",
    "audio/ogg",
    "image/gif",
    "image/jpeg",
    "image/png",
    "image/webp",
    "text/calendar",
    "text/plain",
    "text/vcard",
    "video/mp4",
];

/// The `Content-Type` to send for an attachment stored with `media_type`.
pub fn served_media_type(media_type: &str) -> &'static str {
    SAFE_MEDIA_TYPES
        .iter()
        .find(|safe| **safe == media_type)
        .copied()
        .unwrap_or(OCTET_STREAM)
}

#[cfg(test)]
mod tests;
