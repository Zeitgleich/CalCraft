-- CalDAV scheduling (RFC 6638) between users of this server, see
-- `scheduling.rs` and docs/scheduling.md.

-- Calendar user address set by an administrator (never taken from an SSO
-- identity provider). Stored normalised: ASCII, lower case, without the
-- `mailto:` scheme. Unique across users.
ALTER TABLE users ADD COLUMN email TEXT CHECK (email IS NULL OR (email = lower(email) AND length(email) <= 254));
CREATE UNIQUE INDEX users_by_email ON users (email) WHERE email IS NOT NULL;

-- Revision of the user's scheduling inbox (its getctag), bumped by every
-- delivery and deletion.
ALTER TABLE users ADD COLUMN inbox_revision INTEGER NOT NULL DEFAULT 0;

-- Schedule-Tag (RFC 6638 §3.2.10) of scheduling object resources; NULL for
-- every other object. Changes with every change except merged replies.
ALTER TABLE items ADD COLUMN schedule_tag TEXT;

-- Scheduling inbox: iTIP messages delivered to a user (REQUEST, CANCEL,
-- REPLY). Messages are notifications only: the server has already applied
-- them to the calendars. A newer message from the same sender about the
-- same UID replaces the older one. Not part of the trash: deleting a
-- message is final.
CREATE TABLE schedule_inbox (
    id         INTEGER PRIMARY KEY,
    owner_id   INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- Resource name in the inbox collection, e.g. "<hex>.ics".
    name       TEXT    NOT NULL,
    uid        TEXT    NOT NULL,
    -- Normalised calendar user address of the sender.
    sender     TEXT    NOT NULL,
    etag       TEXT    NOT NULL,
    content    BLOB    NOT NULL,
    -- Unix seconds.
    created_at INTEGER NOT NULL,
    UNIQUE (owner_id, name),
    UNIQUE (owner_id, uid, sender)
) STRICT;

CREATE INDEX schedule_inbox_by_age ON schedule_inbox (owner_id, id);
