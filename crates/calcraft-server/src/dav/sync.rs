//! Sync tokens (RFC 6578 §3.2).
//!
//! A token names the collection it belongs to, so a token from one
//! collection can never be used to list changes of another.

use calcraft_store::Collection;

const PREFIX: &str = "urn:calcraft:sync:";

/// The current sync token of a collection.
pub fn token(collection: &Collection) -> String {
    format!("{PREFIX}{}:{}", collection.id, collection.sync_revision)
}

/// Returns the revision of a token issued for `collection`, or `None` if
/// the token is malformed, belongs to another collection or lies in the
/// future.
pub fn revision(token: &str, collection: &Collection) -> Option<i64> {
    let (id, revision) = token.strip_prefix(PREFIX)?.split_once(':')?;
    let id: i64 = id.parse().ok()?;
    let revision: i64 = revision.parse().ok()?;
    (id == collection.id && (0..=collection.sync_revision).contains(&revision)).then_some(revision)
}

#[cfg(test)]
mod tests {
    use super::*;
    use calcraft_core::CollectionKind;

    fn collection(id: i64, revision: i64) -> Collection {
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
        }
    }

    #[test]
    fn tokens_round_trip_and_are_bound_to_collection() {
        let mine = collection(7, 12);
        assert_eq!(revision(&token(&mine), &mine), Some(12));
        assert_eq!(revision("urn:calcraft:sync:7:3", &mine), Some(3));
        for bad in [
            "urn:calcraft:sync:8:3",
            "urn:calcraft:sync:7:13",
            "urn:calcraft:sync:7:-1",
            "urn:calcraft:sync:7",
            "urn:calcraft:sync:x:1",
            "http://other/sync/7:3",
            "",
        ] {
            assert_eq!(revision(bad, &mine), None, "{bad}");
        }
    }
}
