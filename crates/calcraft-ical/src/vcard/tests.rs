use super::*;

fn card(body: &str) -> Result<VCard, IcalError> {
    VCard::parse(format!("BEGIN:VCARD\r\n{body}END:VCARD\r\n").as_bytes())
}

#[test]
fn accepts_v3_and_v4() {
    let v4 =
        card("VERSION:4.0\r\nUID:urn:uuid:1\r\nFN:Alice Example\r\nEMAIL:alice@example.com\r\n")
            .unwrap();
    assert_eq!(v4.uid, "urn:uuid:1");
    assert_eq!(v4.version, "4.0");
    let v3 =
        card("VERSION:3.0\r\nUID:2\r\nFN:Bob\r\nN:Bob;;;;\r\nTEL;TYPE=CELL:+49 123\r\n").unwrap();
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

#[test]
fn base64_syntax() {
    for good in ["QQ==", "QUI=", "QUJD", "QQ", "QUI", "/+9A"] {
        assert!(is_base64(good), "{good}");
    }
    for bad in [
        "", "=", "Q", "Q===", "QQ=", "QUJD=", "QU JD", "QU-_", "QQ==QQ==", "QUJD\t",
    ] {
        assert!(!is_base64(bad), "{bad}");
    }
}

#[test]
fn accepts_photos_and_logos() {
    for photo in [
        "PHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQ\r\n",
        "PHOTO;ENCODING=BASE64;TYPE=image/png:iVBORw0KGgo=\r\n",
        "PHOTO;ENCODING=b;TYPE=JPEG;X-ABCROP-RECTANGLE=ABClipRect_1&0&0&1&1:/9j/4AAQ\r\n",
        "LOGO;ENCODING=b;TYPE=GIF:R0lGODlh\r\n",
        "PHOTO;VALUE=uri:https://example.com/a.jpg\r\n",
        "PHOTO;ENCODING=b;VALUE=binary;TYPE=JPEG,image/jpeg:/9j/4AAQ\r\n",
        "PHOTO;ENCODING=b;ENCODING=BASE64;TYPE=PNG:iVBORw0KGgo=\r\n",
        // A mislabelled but allowed image type is fine.
        "PHOTO;ENCODING=b;TYPE=JPEG:iVBORw0KGgo=\r\n",
        "PHOTO;ENCODING=b;TYPE=JPEG:/9j/\r\n 4AAQ\r\n",
        // Untyped, recognised by the magic number.
        "PHOTO;ENCODING=b:/9j/4AAQSkZJRg==\r\n",
        "PHOTO;ENCODING=BASE64:iVBORw0KGgoAAAANSUhEUg==\r\n",
        "PHOTO;ENCODING=b:R0lGODlhAQABAA==\r\n",
        "PHOTO;ENCODING=b:UklGRiQAAABXRUJQVlA4IA==\r\n",
        // Empty photos occur in exports.
        "PHOTO:\r\n",
        "PHOTO;ENCODING=b;TYPE=JPEG:\r\n",
        "LOGO;VALUE=uri:\r\n",
    ] {
        card(&format!("VERSION:3.0\r\nUID:1\r\nFN:x\r\n{photo}")).unwrap();
    }
    for photo in [
        "PHOTO:data:image/jpeg;base64,/9j/4AAQ\r\n",
        "PHOTO;MEDIATYPE=image/webp:DATA:image/webp;base64,UklGRiQAAABXRUJQVlA4IA==\r\n",
        "PHOTO:data:image/jpeg\\;base64\\,/9j/4AAQ\r\n",
        "LOGO:http://example.com/logo.png\r\n",
    ] {
        card(&format!("VERSION:4.0\r\nUID:1\r\nFN:x\r\n{photo}")).unwrap();
    }
}

#[test]
fn rejects_unsafe_or_invalid_photos() {
    for photo in [
        "PHOTO;ENCODING=b;TYPE=JPEG:not base64!\r\n",
        "PHOTO;ENCODING=b;TYPE=SVG:PHN2Zz4=\r\n",
        "PHOTO;ENCODING=b:PHN2Zz4=\r\n",
        "PHOTO;ENCODING=b:AAAAAAAA\r\n",
        // Every TYPE counts, also later ones and lists.
        "PHOTO;ENCODING=b;TYPE=JPEG;TYPE=SVG+XML:/9j/4AAQ\r\n",
        "PHOTO;ENCODING=b;TYPE=JPEG,image/svg+xml:/9j/4AAQ\r\n",
        "PHOTO;ENCODING=b;TYPE=\"JPEG,SVG\":/9j/4AAQ\r\n",
        // Conflicting or duplicate ENCODING/VALUE.
        "PHOTO;ENCODING=b;ENCODING=8bit;TYPE=JPEG:/9j/4AAQ\r\n",
        "PHOTO;ENCODING=b;VALUE=uri;TYPE=JPEG:/9j/4AAQ\r\n",
        "PHOTO;VALUE=uri;VALUE=binary:https://example.com/a.jpg\r\n",
        "PHOTO;VALUE=binary:https://example.com/a.jpg\r\n",
        // The declared type does not override the magic number.
        "PHOTO;ENCODING=b;TYPE=JPEG:PHN2ZyBvbmxvYWQ9YWxlcnQoMSk+\r\n",
        "PHOTO:data:image/jpeg;base64,PHN2ZyBvbmxvYWQ9YWxlcnQoMSk+\r\n",
        "PHOTO:data:jpeg;base64,/9j/4AAQ\r\n",
        "PHOTO;ENCODING=QUOTED-PRINTABLE;TYPE=JPEG:abc\r\n",
        "PHOTO:data:image/svg+xml;base64,PHN2Zz4=\r\n",
        "PHOTO:data:text/html;base64,PGI+\r\n",
        "PHOTO:data:image/jpeg,%FF%D8\r\n",
        "PHOTO:data:image/jpeg;base64,***\r\n",
        "PHOTO:data:;base64,/9j/\r\n",
        "PHOTO:javascript:alert(1)\r\n",
        "PHOTO:JavaScript:alert(1)\r\n",
        "PHOTO:file:///etc/passwd\r\n",
        "PHOTO:ftp://example.com/a.jpg\r\n",
        "PHOTO:https://\r\n",
        "PHOTO:https://example.com/a b.jpg\r\n",
        "LOGO:vbscript:x\r\n",
    ] {
        assert!(
            card(&format!("VERSION:4.0\r\nUID:1\r\nFN:x\r\n{photo}")).is_err(),
            "{photo:?}"
        );
    }
}

#[test]
fn groups() {
    let apple = "VERSION:3.0\r\nUID:g1\r\nFN:Family\r\nN:Family;;;;\r\nX-ADDRESSBOOKSERVER-KIND:group\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:a\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:b\r\n";
    assert_eq!(card(apple).unwrap().uid, "g1");
    let v4 = "VERSION:4.0\r\nUID:urn:uuid:g2\r\nFN:Team\r\nKIND:group\r\nMEMBER:urn:uuid:a\r\nMEMBER:mailto:b@example.com\r\n";
    assert_eq!(card(v4).unwrap().uid, "urn:uuid:g2");
    // An empty group is fine.
    card("VERSION:4.0\r\nUID:g3\r\nFN:Empty\r\nKIND:GROUP\r\n").unwrap();
    for bad in [
        "VERSION:4.0\r\nUID:g\r\nFN:x\r\nMEMBER:urn:uuid:a\r\n",
        "VERSION:4.0\r\nUID:g\r\nFN:x\r\nKIND:individual\r\nMEMBER:urn:uuid:a\r\n",
        "VERSION:4.0\r\nUID:g\r\nFN:x\r\nKIND:group\r\nKIND:group\r\n",
    ] {
        assert!(card(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn size_limit_is_larger_than_for_calendars() {
    let photo = format!("/9j/{}", "A".repeat(crate::MAX_OBJECT_SIZE));
    card(&format!(
        "VERSION:3.0\r\nUID:1\r\nFN:x\r\nPHOTO;ENCODING=b;TYPE=JPEG:{photo}\r\n"
    ))
    .unwrap();
    let huge = "A".repeat(crate::MAX_VCARD_SIZE);
    assert_eq!(
        card(&format!(
            "VERSION:3.0\r\nUID:1\r\nFN:x\r\nPHOTO;ENCODING=b;TYPE=JPEG:{huge}\r\n"
        )),
        Err(IcalError::TooLarge)
    );
}

#[test]
fn fuzz_seeds_are_valid() {
    for seed in [
        &include_bytes!("../../../../fuzz/seeds/ical_object/vcard3_photo.vcf")[..],
        include_bytes!("../../../../fuzz/seeds/ical_object/vcard4_photo.vcf"),
        include_bytes!("../../../../fuzz/seeds/ical_object/vcard3_apple_group.vcf"),
        include_bytes!("../../../../fuzz/seeds/ical_object/vcard4_group.vcf"),
    ] {
        VCard::parse(seed).unwrap();
    }
}
