use super::*;

#[test]
fn accepts_valid_names() {
    for name in ["alice", "a", "bob.smith", "user_1", "x-y", "0day"] {
        assert!(name.parse::<Username>().is_ok(), "{name}");
    }
    assert!("a".repeat(64).parse::<Slug>().is_ok());
}

#[test]
fn rejects_invalid_names() {
    for name in [
        "",
        ".",
        "..",
        ".hidden",
        "-x",
        "_x",
        "Alice",
        "a/b",
        "a%2f",
        "a b",
        "ä",
        "a\0",
        "ｆｕｌｌ",
    ] {
        assert!(name.parse::<Username>().is_err(), "{name:?}");
    }
    assert!("a".repeat(65).parse::<Slug>().is_err());
}

#[test]
fn deserialize_validates() {
    assert!(serde_json::from_str::<Username>("\"../x\"").is_err());
    assert_eq!(
        serde_json::from_str::<Username>("\"alice\"")
            .unwrap()
            .as_str(),
        "alice"
    );
}
