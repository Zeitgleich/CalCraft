use super::*;

#[test]
fn requests_reject_unknown_fields() {
    let login: LoginRequest = serde_json::from_str(r#"{"username":"a","password":"b"}"#).unwrap();
    assert_eq!(login.code, None);
    assert!(
        serde_json::from_str::<LoginRequest>(r#"{"username":"a","password":"b","admin":true}"#)
            .is_err()
    );
    assert!(serde_json::from_str::<UpdateUser>(r#"{"is_admin":true,"role":"root"}"#).is_err());
}

#[test]
fn collection_kind_is_lowercase() {
    let create: CreateCollection =
        serde_json::from_str(r#"{"kind":"addressbook","slug":"team"}"#).unwrap();
    assert_eq!(create.kind, CollectionKind::Addressbook);
}
