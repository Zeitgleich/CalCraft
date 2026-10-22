use super::*;
use calcraft_core::CollectionKind;

fn collection(id: i64, salt: i64, floor: i64, revision: i64) -> Collection {
    Collection {
        id,
        owner_id: 1,
        kind: CollectionKind::Calendar,
        slug: "default".parse().unwrap(),
        display_name: None,
        description: None,
        color: None,
        sync_revision: revision,
        components: String::new(),
        sync_floor: floor,
        token_salt: salt,
    }
}

#[test]
fn tokens_round_trip_and_are_bound_to_collection() {
    let mine = collection(7, 4242, 2, 12);
    assert_eq!(revision(&token(&mine), &mine), Some(12));
    assert_eq!(revision("urn:calcraft:sync:7-4242:3", &mine), Some(3));
    for bad in [
        "urn:calcraft:sync:8-4242:3",
        "urn:calcraft:sync:7-1:3",
        "urn:calcraft:sync:7-4242:13",
        "urn:calcraft:sync:7-4242:1",
        "urn:calcraft:sync:7-4242:-1",
        "urn:calcraft:sync:7:3",
        "urn:calcraft:sync:7-4242",
        "http://other/sync/7:3",
        "",
    ] {
        assert_eq!(revision(bad, &mine), None, "{bad}");
    }
    // A new collection reusing the id gets a different salt.
    let reused = collection(7, 9999, 0, 12);
    assert_eq!(revision(&token(&mine), &reused), None);
}
