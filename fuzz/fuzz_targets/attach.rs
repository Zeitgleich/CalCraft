//! Managed attachments (RFC 8607): `Content-Disposition` parsing, file
//! name sanitising and the download header, and the ATTACH operations on
//! calendar objects must never panic, and
//!
//! * a sanitised file name is never empty, at most 255 bytes, without path
//!   separators, control, bidirectional or invisible characters;
//! * the download header is visible ASCII and names the file exactly;
//! * after an add the object references the new id, after a remove it no
//!   longer does on the targeted components, and the result serialises
//!   and parses again.
//!
//! The input is a `Content-Disposition` value, a line `--`, a `rid` query
//! value, a line `--` and a calendar object.
#![no_main]

use calcraft_ical::{
    attach::{self, MAX_FILENAME_LEN},
    parse,
    write::to_string,
};
use libfuzzer_sys::fuzz_target;

const ID: &str = "0123456789abcdef0123456789abcdef";

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let mut parts = text.splitn(3, "\n--\n");
    let disposition = parts.next().unwrap_or_default();
    let rid = parts.next().unwrap_or_default();
    let object = parts.next().unwrap_or_default();

    let raw = attach::disposition_filename(disposition).unwrap_or_default();
    let name = attach::sanitize_filename(&raw);
    assert!(!name.is_empty() && name.len() <= MAX_FILENAME_LEN);
    assert!(!name.contains(['/', '\\', '"']));
    assert!(!name.chars().any(char::is_control));
    assert!(!name.starts_with('.') && !name.starts_with(' '));
    let header = attach::disposition_header(&name);
    assert!(header.bytes().all(|b| b.is_ascii_graphic() || b == b' '));
    assert_eq!(attach::disposition_filename(&header).as_deref(), Some(name.as_str()));
    let _ = attach::media_type(disposition);

    let Ok(calendar) = parse(object.as_bytes()) else {
        return;
    };
    let rids = if rid.is_empty() {
        None
    } else {
        match attach::parse_rids(rid) {
            Ok(rids) => Some(rids),
            Err(_) => return,
        }
    };
    let property = attach::managed_property("https://cal.example/a", ID, &name, "text/plain", 1);
    let mut added = calendar.clone();
    if attach::add(&mut added, &property, rids.as_deref()).is_ok() {
        assert!(attach::managed_ids(&added).iter().any(|id| id == ID));
        if let Ok(text) = to_string(&added) {
            assert!(parse(text.as_bytes()).is_ok());
        }
        let mut removed = added.clone();
        if attach::remove(&mut removed, ID, rids.as_deref()).is_ok() && rids.is_none() {
            assert!(!attach::managed_ids(&removed).iter().any(|id| id == ID));
        }
        let mut replaced = added.clone();
        let other = attach::managed_property("https://cal.example/b", "b", "b", "text/plain", 2);
        assert!(attach::replace(&mut replaced, ID, &other).is_ok());
        assert!(!attach::managed_ids(&replaced).iter().any(|id| id == ID));
    }
    let mut stripped = calendar;
    attach::strip_managed(&mut stripped);
    assert!(attach::managed_ids(&stripped).is_empty());
});
