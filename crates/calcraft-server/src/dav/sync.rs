//! Sync tokens (RFC 6578 §3.2).
//!
//! A token names the collection it belongs to, including a random salt, so
//! a token from one collection can never be used for another – not even
//! for a new collection that reuses the id of a deleted one.

use calcraft_store::Collection;

const PREFIX: &str = "urn:calcraft:sync:";

/// A sync token for `collection` at `revision`.
pub fn token_at(collection: &Collection, revision: i64) -> String {
    format!(
        "{PREFIX}{}-{}:{revision}",
        collection.id, collection.token_salt
    )
}

/// The current sync token of a collection.
pub fn token(collection: &Collection) -> String {
    token_at(collection, collection.sync_revision)
}

/// Returns the revision of a token issued for `collection`, or `None` if
/// the token is malformed, belongs to another collection, lies in the
/// future or is older than the retained change history.
pub fn revision(token: &str, collection: &Collection) -> Option<i64> {
    let (identity, revision) = token.strip_prefix(PREFIX)?.split_once(':')?;
    let (id, salt) = identity.split_once('-')?;
    let id: i64 = id.parse().ok()?;
    let salt: i64 = salt.parse().ok()?;
    let revision: i64 = revision.parse().ok()?;
    let valid = id == collection.id
        && salt == collection.token_salt
        && (collection.sync_floor..=collection.sync_revision).contains(&revision);
    valid.then_some(revision)
}

#[cfg(test)]
mod tests {
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
}
