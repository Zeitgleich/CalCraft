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

#[test]
fn group_names_follow_the_username_rules() {
    for name in ["staff", "team.a", "g_1", "x-y"] {
        assert!(name.parse::<GroupName>().is_ok(), "{name}");
    }
    for name in ["", ".", "..", "-x", "Staff", "a/b", "a,b", "a b", "ä"] {
        assert!(name.parse::<GroupName>().is_err(), "{name:?}");
    }
    assert!("a".repeat(64).parse::<GroupName>().is_ok());
    assert!("a".repeat(65).parse::<GroupName>().is_err());
}
