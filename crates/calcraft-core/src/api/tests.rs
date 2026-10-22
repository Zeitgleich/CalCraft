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

#[test]
fn sso_fields_are_optional_for_older_servers() {
    let me: Me = serde_json::from_str(
        r#"{"username":"a","display_name":null,"is_admin":false,"totp_enabled":false}"#,
    )
    .unwrap();
    assert_eq!(me.sso, None);
    let user: AdminUser = serde_json::from_str(
        r#"{"username":"a","display_name":null,"is_admin":false,"disabled":false,"totp_enabled":false}"#,
    )
    .unwrap();
    assert!(!user.sso_linked);
    let link: SsoLink =
        serde_json::from_str(r#"{"linked":false,"issuer":"https://idp.example"}"#).unwrap();
    assert_eq!(link.linked_at, None);
    assert_eq!(link.last_login_at, None);
}
