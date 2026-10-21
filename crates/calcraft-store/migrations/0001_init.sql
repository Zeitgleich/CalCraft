CREATE TABLE users (
    id            INTEGER PRIMARY KEY,
    username      TEXT    NOT NULL UNIQUE,
    password_hash TEXT    NOT NULL,
    display_name  TEXT,
    disabled      INTEGER NOT NULL DEFAULT 0 CHECK (disabled IN (0, 1)),
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
) STRICT;

CREATE TABLE collections (
    id            INTEGER PRIMARY KEY,
    owner_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind          TEXT    NOT NULL CHECK (kind IN ('calendar', 'addressbook')),
    slug          TEXT    NOT NULL,
    display_name  TEXT,
    description   TEXT,
    color         TEXT,
    -- Incremented on every change; basis for getctag and sync tokens.
    sync_revision INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE (owner_id, kind, slug)
) STRICT;
