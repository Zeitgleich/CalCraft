use super::*;

#[test]
fn base32_matches_rfc_4648_vectors() {
    for (input, expected) in [
        ("", ""),
        ("f", "MY"),
        ("fo", "MZXQ"),
        ("foo", "MZXW6"),
        ("foob", "MZXW6YQ"),
        ("fooba", "MZXW6YTB"),
        ("foobar", "MZXW6YTBOI"),
    ] {
        assert_eq!(base32_encode(input.as_bytes()), expected, "{input}");
        assert_eq!(
            base32_decode(expected).unwrap(),
            input.as_bytes(),
            "{input}"
        );
    }
    assert_eq!(base32_decode("mzxw6").unwrap(), b"foo");
    assert!(base32_decode("MZ1W").is_none());
}

#[test]
fn totp_matches_rfc_6238_vectors() {
    // RFC 6238 Appendix B, SHA-1 key, last six of the eight digits.
    let secret = b"12345678901234567890";
    for (time, code) in [
        (59, "287082"),
        (1_111_111_109, "081804"),
        (1_111_111_111, "050471"),
        (1_234_567_890, "005924"),
        (2_000_000_000, "279037"),
    ] {
        assert_eq!(totp_code(secret, time / TOTP_STEP), code, "t={time}");
    }
}

#[test]
fn totp_verify_allows_one_step_of_drift() {
    let secret = b"12345678901234567890";
    let now = 1_111_111_111;
    let step = now / TOTP_STEP;
    assert_eq!(totp_verify(secret, "050471", now), Some(step));
    assert_eq!(
        totp_verify(secret, &totp_code(secret, step - 1), now),
        Some(step - 1)
    );
    assert_eq!(
        totp_verify(secret, &totp_code(secret, step + 1), now),
        Some(step + 1)
    );
    assert_eq!(totp_verify(secret, &totp_code(secret, step + 2), now), None);
    for bad in ["", "12345", "1234567", "abcdef", "05047 "] {
        assert_eq!(totp_verify(secret, bad, now), None, "{bad:?}");
    }
}

#[test]
fn hashes_and_tokens() {
    let a = hash_secret("token");
    assert!(hashes_equal(&a, &hash_secret("token")));
    assert!(!hashes_equal(&a, &hash_secret("other")));
    assert!(!hashes_equal("zz", "zz"));
    let token = random_token::<20>();
    assert_eq!(token.len(), 32);
    assert_ne!(token, random_token::<20>());
}
