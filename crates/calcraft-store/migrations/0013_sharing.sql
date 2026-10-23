-- Sharing of calendars and address books between users, and public
-- read-only links of calendars, see `sharing.rs`, `publish.rs` and
-- docs/sharing.md.

-- One collection shared by its owner with another user (the sharee).
-- Pending until the sharee accepts; only then does the collection appear in
-- the sharee's home, below `slug`. Declining, unsubscribing or the owner
-- removing the share deletes the row; deleting the collection or either
-- user deletes it too (foreign keys). Shares of a disabled owner or sharee
-- stay stored but are ignored by every query.
CREATE TABLE shares (
    id            INTEGER PRIMARY KEY,
    -- Random id (128 bits, lower-case hex) for the API and the sharee's
    -- invitation notification; never derived from other data.
    public_id     TEXT    NOT NULL UNIQUE CHECK (length(public_id) = 32),
    collection_id INTEGER NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    sharee_id     INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- Kind of the collection (copied), for the uniqueness of `slug` in the
    -- sharee's calendar or address book home.
    kind          TEXT    NOT NULL CHECK (kind IN ('calendar', 'addressbook')),
    access        TEXT    NOT NULL CHECK (access IN ('read', 'read-write')),
    -- Path segment in the sharee's home; NULL while pending.
    slug          TEXT,
    -- The sharee's own name, colour and position of the collection; NULL:
    -- the owner's name and colour, no position.
    display_name  TEXT,
    color         TEXT,
    sort_order    INTEGER,
    -- Unix seconds.
    created_at    INTEGER NOT NULL,
    accepted_at   INTEGER,
    UNIQUE (collection_id, sharee_id),
    CHECK ((slug IS NULL) = (accepted_at IS NULL))
) STRICT;

CREATE UNIQUE INDEX shares_by_sharee_slug ON shares (sharee_id, kind, slug)
    WHERE slug IS NOT NULL;
CREATE INDEX shares_by_sharee ON shares (sharee_id, accepted_at);

-- A secret, read-only link to a calendar (`/public/<token>.ics`). Only a
-- hash of the token is stored; the token is shown once when the link is
-- created.
CREATE TABLE publish_links (
    id                INTEGER PRIMARY KEY,
    -- Random id (128 bits, lower-case hex) for the API; not the secret.
    public_id         TEXT    NOT NULL UNIQUE CHECK (length(public_id) = 32),
    collection_id     INTEGER NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    -- BLAKE3 of the token (256 random bits), lower-case hex.
    token_hash        TEXT    NOT NULL UNIQUE CHECK (length(token_hash) = 64),
    -- Only busy blocks, no titles or other details.
    busy_only         INTEGER NOT NULL CHECK (busy_only IN (0, 1)),
    -- Events with CLASS:PRIVATE or CLASS:CONFIDENTIAL: shown as busy
    -- blocks, left out, or shown like the others.
    private_events    TEXT    NOT NULL CHECK (private_events IN ('busy', 'hide', 'show')),
    -- ORGANIZER and ATTENDEE (e-mail addresses) are left out unless set.
    include_attendees INTEGER NOT NULL CHECK (include_attendees IN (0, 1)),
    created_at        INTEGER NOT NULL
) STRICT;

CREATE INDEX publish_links_by_collection ON publish_links (collection_id);

-- Invitations a sharee declined or shares they left, per owner: the owner
-- cannot invite them again for a day (`sharing.rs`), so declining cannot
-- be answered with a flood of new invitations.
CREATE TABLE share_declines (
    owner_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    sharee_id   INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- Unix seconds.
    declined_at INTEGER NOT NULL,
    PRIMARY KEY (owner_id, sharee_id)
) STRICT;

-- Whether an object has a component with a CLASS other than PUBLIC.
-- Read-only sharees may not download the attachments of such objects
-- (their events are shown to them as busy stubs). Computed by the
-- triggers below from the content with folded lines unfolded (CRLF or LF); a CLASS line
-- with parameters counts as private (conservative). The server decides
-- what a sharee sees from the parsed content, this flag only guards the
-- attachment downloads.
ALTER TABLE items ADD COLUMN private INTEGER NOT NULL DEFAULT 0 CHECK (private IN (0, 1));

-- The same expression three times: number of CLASS lines greater than
-- the number of CLASS:PUBLIC lines (content unfolded, upper-cased).
UPDATE items SET private = (length((char(10) || upper(replace(replace(replace(replace(CAST(content AS TEXT), char(13, 10, 32), ''), char(13, 10, 9), ''), char(10, 32), ''), char(10, 9), '')))) - length(replace((char(10) || upper(replace(replace(replace(replace(CAST(content AS TEXT), char(13, 10, 32), ''), char(13, 10, 9), ''), char(10, 32), ''), char(10, 9), ''))), char(10) || 'CLASS', ''))) / 6
        > (length((char(10) || upper(replace(replace(replace(replace(CAST(content AS TEXT), char(13, 10, 32), ''), char(13, 10, 9), ''), char(10, 32), ''), char(10, 9), '')))) - length(replace((char(10) || upper(replace(replace(replace(replace(CAST(content AS TEXT), char(13, 10, 32), ''), char(13, 10, 9), ''), char(10, 32), ''), char(10, 9), ''))), char(10) || 'CLASS:PUBLIC' || char(13), ''))) / 14;

CREATE TRIGGER items_private_insert AFTER INSERT ON items
BEGIN
    UPDATE items SET private = (length((char(10) || upper(replace(replace(replace(replace(CAST(NEW.content AS TEXT), char(13, 10, 32), ''), char(13, 10, 9), ''), char(10, 32), ''), char(10, 9), '')))) - length(replace((char(10) || upper(replace(replace(replace(replace(CAST(NEW.content AS TEXT), char(13, 10, 32), ''), char(13, 10, 9), ''), char(10, 32), ''), char(10, 9), ''))), char(10) || 'CLASS', ''))) / 6
        > (length((char(10) || upper(replace(replace(replace(replace(CAST(NEW.content AS TEXT), char(13, 10, 32), ''), char(13, 10, 9), ''), char(10, 32), ''), char(10, 9), '')))) - length(replace((char(10) || upper(replace(replace(replace(replace(CAST(NEW.content AS TEXT), char(13, 10, 32), ''), char(13, 10, 9), ''), char(10, 32), ''), char(10, 9), ''))), char(10) || 'CLASS:PUBLIC' || char(13), ''))) / 14
    WHERE id = NEW.id;
END;

CREATE TRIGGER items_private_update AFTER UPDATE OF content ON items
BEGIN
    UPDATE items SET private = (length((char(10) || upper(replace(replace(replace(replace(CAST(NEW.content AS TEXT), char(13, 10, 32), ''), char(13, 10, 9), ''), char(10, 32), ''), char(10, 9), '')))) - length(replace((char(10) || upper(replace(replace(replace(replace(CAST(NEW.content AS TEXT), char(13, 10, 32), ''), char(13, 10, 9), ''), char(10, 32), ''), char(10, 9), ''))), char(10) || 'CLASS', ''))) / 6
        > (length((char(10) || upper(replace(replace(replace(replace(CAST(NEW.content AS TEXT), char(13, 10, 32), ''), char(13, 10, 9), ''), char(10, 32), ''), char(10, 9), '')))) - length(replace((char(10) || upper(replace(replace(replace(replace(CAST(NEW.content AS TEXT), char(13, 10, 32), ''), char(13, 10, 9), ''), char(10, 32), ''), char(10, 9), ''))), char(10) || 'CLASS:PUBLIC' || char(13), ''))) / 14
    WHERE id = NEW.id;
END;
