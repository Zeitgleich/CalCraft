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
mod tests;
