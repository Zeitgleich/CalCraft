use super::*;

async fn setup() -> (Store, User, User) {
    let store = Store::in_memory().await.unwrap();
    let alice = store
        .create_user(&"alice".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let bob = store
        .create_user(&"bob".parse().unwrap(), "h", None)
        .await
        .unwrap();
    (store, alice, bob)
}

#[tokio::test]
async fn sessions_expire_and_are_scoped() {
    let (store, alice, _) = setup().await;
    store
        .create_session(alice.id, "hash-a", 100, 200)
        .await
        .unwrap();
    assert_eq!(
        store.session("hash-a", 150).await.unwrap().unwrap().user.id,
        alice.id
    );
    assert!(store.session("hash-a", 200).await.unwrap().is_none());
    assert!(store.session("other", 150).await.unwrap().is_none());
    store.delete_sessions_of(alice.id).await.unwrap();
    assert!(store.session("hash-a", 150).await.unwrap().is_none());
}

#[tokio::test]
async fn app_passwords_belong_to_their_user() {
    let (store, alice, bob) = setup().await;
    let id = store
        .create_app_password(alice.id, "Phone", "secret-hash", 10)
        .await
        .unwrap();
    assert_eq!(store.app_passwords(alice.id).await.unwrap().len(), 1);
    assert!(store.app_passwords(bob.id).await.unwrap().is_empty());
    // Bob cannot delete Alice's app password.
    assert!(!store.delete_app_password(bob.id, id).await.unwrap());
    assert_eq!(
        store.app_password_secret(id).await.unwrap().unwrap().0,
        alice.id
    );

    store.touch_app_password(id, 5000).await.unwrap();
    store.touch_app_password(id, 5100).await.unwrap();
    assert_eq!(
        store.app_passwords(alice.id).await.unwrap()[0].last_used_at,
        Some(5000)
    );
    assert!(store.delete_app_password(alice.id, id).await.unwrap());
}

#[tokio::test]
async fn totp_lifecycle_and_replay_protection() {
    let (store, alice, _) = setup().await;
    assert!(store.totp_state(alice.id).await.unwrap().is_none());
    assert!(store.set_pending_totp(alice.id, b"secret").await.unwrap());
    assert!(!store.totp_state(alice.id).await.unwrap().unwrap().enabled);
    store
        .enable_totp(alice.id, &["c1".into(), "c2".into()])
        .await
        .unwrap();
    assert!(store.totp_state(alice.id).await.unwrap().unwrap().enabled);
    // Cannot be replaced while enabled.
    assert!(!store.set_pending_totp(alice.id, b"other").await.unwrap());

    assert!(store.accept_totp_step(alice.id, 10).await.unwrap());
    assert!(!store.accept_totp_step(alice.id, 10).await.unwrap());
    assert!(!store.accept_totp_step(alice.id, 9).await.unwrap());
    assert!(store.accept_totp_step(alice.id, 11).await.unwrap());

    assert!(store.use_recovery_code(alice.id, "c1").await.unwrap());
    assert!(!store.use_recovery_code(alice.id, "c1").await.unwrap());
    assert_eq!(store.recovery_codes_left(alice.id).await.unwrap(), 1);

    store.disable_totp(alice.id).await.unwrap();
    assert!(store.totp_state(alice.id).await.unwrap().is_none());
    assert_eq!(store.recovery_codes_left(alice.id).await.unwrap(), 0);
}
