-- Change log for sync-collection (RFC 6578). Only the latest change per
-- object is kept; deletions are kept as tombstones so clients learn about
-- them. When old tombstones are pruned, sync_floor records the oldest
-- revision a sync token may still refer to.
CREATE TABLE changes (
    collection_id INTEGER NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    name          TEXT    NOT NULL,
    revision      INTEGER NOT NULL,
    deleted       INTEGER NOT NULL CHECK (deleted IN (0, 1)),
    PRIMARY KEY (collection_id, name)
) STRICT;

CREATE INDEX changes_by_revision ON changes (collection_id, revision);

ALTER TABLE collections ADD COLUMN sync_floor INTEGER NOT NULL DEFAULT 0;
-- Random per-collection value in sync tokens: collection ids can be reused
-- after a deletion, tokens of a deleted collection must never match.
ALTER TABLE collections ADD COLUMN token_salt INTEGER NOT NULL DEFAULT 0;
UPDATE collections SET token_salt = abs(random());
