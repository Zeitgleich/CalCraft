-- Component types a calendar accepts (supported-calendar-component-set).
ALTER TABLE collections ADD COLUMN components TEXT NOT NULL DEFAULT 'VEVENT,VTODO';

CREATE TABLE items (
    id            INTEGER PRIMARY KEY,
    collection_id INTEGER NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    -- Last path segment of the resource, e.g. "abc.ics".
    name          TEXT    NOT NULL,
    uid           TEXT    NOT NULL,
    etag          TEXT    NOT NULL,
    component     TEXT    NOT NULL,
    -- Conservative time span in Unix seconds for time-range queries; NULL = unbounded.
    range_start   INTEGER,
    range_end     INTEGER,
    -- Stored exactly as uploaded.
    content       BLOB    NOT NULL,
    -- Collection revision of the last change.
    revision      INTEGER NOT NULL,
    modified_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE (collection_id, name),
    UNIQUE (collection_id, uid)
) STRICT;

CREATE INDEX items_by_range ON items (collection_id, component, range_start, range_end);
