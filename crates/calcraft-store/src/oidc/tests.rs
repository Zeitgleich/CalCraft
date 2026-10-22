use super::*;

#[tokio::test]
async fn identity_info_and_linked_users() {
    let store = Store::in_memory().await.unwrap();
    let alice = store
        .create_user(&"alice".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let bob = store
        .create_user(&"bob".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let iss = "https://idp.example";
    assert_eq!(store.oidc_identity_info(alice.id, iss).await.unwrap(), None);
    assert!(store.oidc_linked_user_ids().await.unwrap().is_empty());

    store
        .bind_oidc_identity(alice.id, iss, "s1", 5)
        .await
        .unwrap();
    store
        .bind_oidc_identity(alice.id, iss, "s1", 9)
        .await
        .unwrap();
    store
        .bind_oidc_identity(alice.id, "https://other.example", "s1", 7)
        .await
        .unwrap();
    assert_eq!(
        store.oidc_identity_info(alice.id, iss).await.unwrap(),
        Some(OidcIdentityInfo {
            created_at: 5,
            last_login_at: 9
        })
    );
    // Per user and per issuer.
    assert_eq!(store.oidc_identity_info(bob.id, iss).await.unwrap(), None);
    assert_eq!(
        store
            .oidc_identity_info(alice.id, "https://third.example")
            .await
            .unwrap(),
        None
    );
    let linked = store.oidc_linked_user_ids().await.unwrap();
    assert_eq!(linked.len(), 1);
    assert!(linked.contains(&alice.id) && !linked.contains(&bob.id));

    assert_eq!(store.unbind_oidc_identities(alice.id).await.unwrap(), 2);
    assert!(store.oidc_linked_user_ids().await.unwrap().is_empty());
}

#[tokio::test]
async fn identities_bind_once_and_are_unique() {
    let store = Store::in_memory().await.unwrap();
    let alice = store
        .create_user(&"alice".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let bob = store
        .create_user(&"bob".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let iss = "https://idp.example";
    assert_eq!(
        store
            .bind_oidc_identity(alice.id, iss, "s1", 1)
            .await
            .unwrap(),
        OidcBinding::Bound
    );
    assert_eq!(
        store
            .bind_oidc_identity(alice.id, iss, "s1", 2)
            .await
            .unwrap(),
        OidcBinding::Matches
    );
    // Another subject for the same user, or the same subject for another user.
    assert_eq!(
        store
            .bind_oidc_identity(alice.id, iss, "s2", 3)
            .await
            .unwrap(),
        OidcBinding::Mismatch
    );
    assert_eq!(
        store
            .bind_oidc_identity(bob.id, iss, "s1", 3)
            .await
            .unwrap(),
        OidcBinding::Mismatch
    );
    // Another issuer is a separate identity.
    assert_eq!(
        store
            .bind_oidc_identity(bob.id, "https://other.example", "s1", 3)
            .await
            .unwrap(),
        OidcBinding::Bound
    );
    assert_eq!(
        store.user_id_by_oidc_identity(iss, "s1").await.unwrap(),
        Some(alice.id)
    );
    assert!(store.has_oidc_identity(alice.id, iss).await.unwrap());
    assert!(
        !store
            .has_oidc_identity(alice.id, "https://other.example")
            .await
            .unwrap()
    );

    // Approvals are bound to issuer + subject, single use and expire.
    let carol = store
        .create_user(&"carol".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let bind = |sub: &'static str, now: i64| {
        let store = store.clone();
        async move {
            store
                .bind_approved_oidc_identity(carol.id, iss, sub, now)
                .await
                .unwrap()
        }
    };
    assert_eq!(bind("c1", 10).await, ApprovedBinding::NotApproved);
    store
        .approve_oidc_link(carol.id, iss, "c1", 100)
        .await
        .unwrap();
    assert_eq!(bind("other", 10).await, ApprovedBinding::NotApproved);
    // Subject already bound to alice: refused, approval kept.
    store
        .approve_oidc_link(carol.id, iss, "s1", 100)
        .await
        .unwrap();
    assert_eq!(bind("s1", 10).await, ApprovedBinding::Mismatch);
    assert_eq!(bind("s1", 10).await, ApprovedBinding::Mismatch);
    assert_eq!(bind("s1", 100).await, ApprovedBinding::NotApproved);
    store
        .approve_oidc_link(carol.id, iss, "c1", 100)
        .await
        .unwrap();
    assert_eq!(bind("c1", 10).await, ApprovedBinding::Bound);
    assert_eq!(bind("c1", 10).await, ApprovedBinding::NotApproved);
    assert_eq!(
        store.user_id_by_oidc_identity(iss, "c1").await.unwrap(),
        Some(carol.id)
    );

    assert_eq!(store.unbind_oidc_identities(alice.id).await.unwrap(), 1);
    assert_eq!(
        store.user_id_by_oidc_identity(iss, "s1").await.unwrap(),
        None
    );
}
