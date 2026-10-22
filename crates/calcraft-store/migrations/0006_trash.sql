-- Trash (soft delete). A DAV or API DELETE moves the object, or the whole
-- collection with its objects, here instead of destroying it; the change
-- log still records the deletion, so sync clients see it as deleted.
-- Entries are purged after `storage.trash_retention_days`.
--
-- Top-level entries (parent_id IS NULL) are what users see and restore. The
-- objects of a deleted collection are rows with parent_id pointing at the
-- collection row: they are restored and purged together with it.
CREATE TABLE trash (
    id               INTEGER PRIMARY KEY,
    -- Random identifier (128 bits, hex) used by the API; NULL for the
    -- objects inside a collection entry, which are never addressed alone.
    public_id        TEXT    UNIQUE,
    owner_id         INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    entry_kind       TEXT    NOT NULL CHECK (entry_kind IN ('item', 'collection')),
    parent_id        INTEGER REFERENCES trash (id) ON DELETE CASCADE,
    -- The collection the entry was deleted from (or the deleted collection).
    collection_kind  TEXT    NOT NULL CHECK (collection_kind IN ('calendar', 'addressbook')),
    collection_slug  TEXT    NOT NULL,
    -- Top-level objects: the live collection they came from. Becomes NULL
    -- when that collection is deleted; if it went to the trash as well,
    -- collection_entry points at its entry until it is restored.
    collection_id    INTEGER REFERENCES collections (id) ON DELETE SET NULL,
    collection_entry INTEGER REFERENCES trash (id) ON DELETE SET NULL,
    -- Objects, exactly as stored before.
    name             TEXT,
    uid              TEXT,
    etag             TEXT,
    component        TEXT,
    range_start      INTEGER,
    range_end        INTEGER,
    content          BLOB,
    -- Collections: their properties.
    display_name     TEXT,
    description      TEXT,
    color            TEXT,
    components       TEXT,
    -- SUMMARY / FN / display name, for listings only.
    summary          TEXT,
    -- Bytes of content (collections: of all their objects).
    size             INTEGER NOT NULL,
    -- Unix seconds.
    deleted_at       INTEGER NOT NULL,
    CHECK ((entry_kind = 'item') = (content IS NOT NULL)),
    CHECK (parent_id IS NULL OR entry_kind = 'item'),
    CHECK ((public_id IS NULL) = (parent_id IS NOT NULL))
) STRICT;

-- Listing and retention.
CREATE INDEX trash_by_owner ON trash (owner_id, deleted_at) WHERE parent_id IS NULL;
CREATE INDEX trash_by_deleted_at ON trash (deleted_at) WHERE parent_id IS NULL;
-- Foreign keys (ON DELETE actions look rows up by these columns).
CREATE INDEX trash_by_parent ON trash (parent_id);
CREATE INDEX trash_by_collection ON trash (collection_id);
CREATE INDEX trash_by_collection_entry ON trash (collection_entry);
-- Quota and entry counts, and ON DELETE CASCADE from users.
CREATE INDEX trash_rows_by_owner ON trash (owner_id, entry_kind, size);
