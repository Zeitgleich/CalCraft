//! Dead properties of collections (RFC 4918 §4).
//!
//! The store keeps them as opaque text: validation and the canonical XML
//! serialisation of values are done by the DAV layer (calcraft-dav
//! `XmlFragment`), which also re-validates every value it reads back.

use calcraft_core::CollectionKind;
use sqlx::{Row, Sqlite, Transaction};

use crate::{Result, Store, StoreError};

/// Dead properties per collection.
pub const MAX_DEAD_PROPERTIES: i64 = 64;
/// Bytes of all dead properties (namespaces, names and values) per collection.
pub const MAX_DEAD_PROPERTY_BYTES: i64 = 64 * 1024;

/// A stored dead property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadProperty {
    /// Namespace URI; empty for properties without namespace.
    pub namespace: String,
    pub name: String,
    /// Serialised XML content of the property element.
    pub value: String,
}

/// Sets (`Some`) or removes (`None`) one dead property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropertyChange<'a> {
    pub namespace: &'a str,
    pub name: &'a str,
    pub value: Option<&'a str>,
}

fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<DeadProperty> {
    Ok(DeadProperty {
        namespace: row.try_get("namespace")?,
        name: row.try_get("name")?,
        value: row.try_get("value")?,
    })
}

impl Store {
    /// The dead properties of a collection, ordered by name.
    pub async fn dead_properties(&self, collection_id: i64) -> Result<Vec<DeadProperty>> {
        let rows = sqlx::query(
            "SELECT namespace, name, value FROM dead_properties \
             WHERE collection_id = ? ORDER BY namespace, name",
        )
        .bind(collection_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(from_row).collect()
    }

    /// The dead properties of all collections of one kind owned by a user,
    /// with the id of their collection.
    pub async fn dead_properties_of_owner(
        &self,
        owner_id: i64,
        kind: CollectionKind,
    ) -> Result<Vec<(i64, DeadProperty)>> {
        let rows = sqlx::query(
            "SELECT p.collection_id, p.namespace, p.name, p.value \
             FROM dead_properties p JOIN collections c ON c.id = p.collection_id \
             WHERE c.owner_id = ? AND c.kind = ? \
             ORDER BY p.collection_id, p.namespace, p.name",
        )
        .bind(owner_id)
        .bind(kind.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| Ok((row.try_get("collection_id")?, from_row(row)?)))
            .collect()
    }
}

/// Applies `changes` in order and enforces the per-collection limits; on
/// [`StoreError::Limit`] the caller's transaction must be rolled back.
pub(crate) async fn apply(
    tx: &mut Transaction<'static, Sqlite>,
    collection_id: i64,
    changes: &[PropertyChange<'_>],
) -> Result<()> {
    if changes.is_empty() {
        return Ok(());
    }
    for change in changes {
        let query = match change.value {
            Some(value) => sqlx::query(
                "INSERT INTO dead_properties (collection_id, namespace, name, value) \
                 VALUES (?, ?, ?, ?) \
                 ON CONFLICT (collection_id, namespace, name) DO UPDATE SET value = excluded.value",
            )
            .bind(collection_id)
            .bind(change.namespace)
            .bind(change.name)
            .bind(value),
            None => sqlx::query(
                "DELETE FROM dead_properties WHERE collection_id = ? AND namespace = ? AND name = ?",
            )
            .bind(collection_id)
            .bind(change.namespace)
            .bind(change.name),
        };
        query.execute(&mut **tx).await?;
    }
    let row = sqlx::query(
        "SELECT COUNT(*) AS n, \
                COALESCE(SUM(length(CAST(namespace AS BLOB)) + length(CAST(name AS BLOB)) \
                             + length(CAST(value AS BLOB))), 0) AS bytes \
         FROM dead_properties WHERE collection_id = ?",
    )
    .bind(collection_id)
    .fetch_one(&mut **tx)
    .await?;
    let count: i64 = row.try_get("n")?;
    let bytes: i64 = row.try_get("bytes")?;
    if count > MAX_DEAD_PROPERTIES {
        return Err(StoreError::Limit("too many dead properties"));
    }
    if bytes > MAX_DEAD_PROPERTY_BYTES {
        return Err(StoreError::Limit("dead properties too large"));
    }
    Ok(())
}

/// Copies the dead properties of a collection to its trash entry.
pub(crate) async fn to_trash(
    tx: &mut Transaction<'static, Sqlite>,
    collection_id: i64,
    entry_id: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO trash_dead_properties (trash_id, namespace, name, value) \
         SELECT ?, namespace, name, value FROM dead_properties WHERE collection_id = ?",
    )
    .bind(entry_id)
    .bind(collection_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Copies the dead properties of a trash entry to its restored collection.
pub(crate) async fn from_trash(
    tx: &mut Transaction<'static, Sqlite>,
    entry_id: i64,
    collection_id: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO dead_properties (collection_id, namespace, name, value) \
         SELECT ?, namespace, name, value FROM trash_dead_properties WHERE trash_id = ?",
    )
    .bind(collection_id)
    .bind(entry_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests;
