-- Change log for sync-collection (RFC 6578). One row per object change;
-- deletions are kept as tombstones so clients learn about them.
CREATE TABLE changes (
    collection_id INTEGER NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    revision      INTEGER NOT NULL,
    name          TEXT    NOT NULL,
    deleted       INTEGER NOT NULL CHECK (deleted IN (0, 1)),
    PRIMARY KEY (collection_id, revision, name)
) STRICT;

CREATE INDEX changes_by_name ON changes (collection_id, name, revision);
