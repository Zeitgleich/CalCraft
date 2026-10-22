-- Dead properties (RFC 4918 §4) of collections: client-defined properties
-- such as Apple's calendar-order, stored by PROPPATCH, MKCALENDAR and
-- extended MKCOL and returned by PROPFIND. `value` is the canonical
-- serialisation of the parsed property content (calcraft-dav
-- `XmlFragment`), never the bytes the client sent. Count and size per
-- collection are capped in `Store::update_collection_with_properties`.
CREATE TABLE dead_properties (
    collection_id INTEGER NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    -- Namespace URI; '' for properties without namespace.
    namespace     TEXT    NOT NULL,
    name          TEXT    NOT NULL,
    value         TEXT    NOT NULL,
    PRIMARY KEY (collection_id, namespace, name)
) STRICT, WITHOUT ROWID;

-- The dead properties of collections in the trash, restored with them.
CREATE TABLE trash_dead_properties (
    trash_id  INTEGER NOT NULL REFERENCES trash (id) ON DELETE CASCADE,
    namespace TEXT    NOT NULL,
    name      TEXT    NOT NULL,
    value     TEXT    NOT NULL,
    PRIMARY KEY (trash_id, namespace, name)
) STRICT, WITHOUT ROWID;
