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
