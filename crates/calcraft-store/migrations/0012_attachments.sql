-- CalDAV managed attachments (RFC 8607), see `attachments.rs` and
-- docs/attachments.md.

-- One uploaded file. Never changed after the upload: an update stores a
-- new row with a new managed id. Owned (and paid for in the quota) by the
-- user who uploaded it.
CREATE TABLE attachments (
    id         INTEGER PRIMARY KEY,
    -- MANAGED-ID parameter and last segment of the download URL: 128
    -- random bits, lower-case hex.
    managed_id TEXT    NOT NULL UNIQUE CHECK (length(managed_id) = 32),
    owner_id   INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- Sanitised FILENAME and validated FMTTYPE.
    filename   TEXT    NOT NULL,
    media_type TEXT    NOT NULL,
    size       INTEGER NOT NULL CHECK (size = length(content)),
    content    BLOB    NOT NULL,
    -- Unix seconds. An upload that no object references (a failed or
    -- aborted POST) is removed after an hour.
    created_at INTEGER NOT NULL
) STRICT;

CREATE INDEX attachments_by_owner ON attachments (owner_id, size);
CREATE INDEX attachments_by_age ON attachments (created_at);

-- Which live object or trash entry references an attachment. Access for
-- users other than the owner, and the lifetime of the attachment, derive
-- from these rows only, never from the ATTACH properties in the content:
-- only the store writes them, after checking that the object may
-- reference the attachment.
CREATE TABLE attachment_refs (
    attachment_id INTEGER NOT NULL REFERENCES attachments (id) ON DELETE CASCADE,
    item_id       INTEGER REFERENCES items (id) ON DELETE CASCADE,
    trash_id      INTEGER REFERENCES trash (id) ON DELETE CASCADE,
    CHECK ((item_id IS NULL) <> (trash_id IS NULL))
) STRICT;

CREATE UNIQUE INDEX attachment_refs_by_item ON attachment_refs (item_id, attachment_id)
    WHERE item_id IS NOT NULL;
CREATE UNIQUE INDEX attachment_refs_by_trash ON attachment_refs (trash_id, attachment_id)
    WHERE trash_id IS NOT NULL;
CREATE INDEX attachment_refs_by_attachment ON attachment_refs (attachment_id);

-- Garbage collection: an attachment goes with its last reference, however
-- that disappears (PUT without the ATTACH, DELETE without trash, purged
-- trash, deleted collection or user; foreign-key cascades fire this too).
CREATE TRIGGER attachment_refs_collect AFTER DELETE ON attachment_refs
WHEN NOT EXISTS (SELECT 1 FROM attachment_refs WHERE attachment_id = OLD.attachment_id)
BEGIN
    DELETE FROM attachments WHERE id = OLD.attachment_id;
END;
