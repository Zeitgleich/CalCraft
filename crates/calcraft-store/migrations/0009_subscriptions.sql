-- Calendar subscriptions (webcal/ICS feeds), see `subscriptions.rs`.
--
-- A subscription is an ordinary calendar collection marked read_only: the
-- server fetches the feed and stores its events as normal objects, so every
-- CalDAV client sees a read-only calendar (including sync-collection).
-- Writes through DAV, the REST API and the trash are refused for read-only
-- collections; only the refresher changes their objects.
ALTER TABLE collections ADD COLUMN read_only INTEGER NOT NULL DEFAULT 0 CHECK (read_only IN (0, 1));

CREATE TABLE subscriptions (
    collection_id      INTEGER PRIMARY KEY REFERENCES collections (id) ON DELETE CASCADE,
    -- Feed URL as entered by the owner (webcal:// already turned into
    -- https://). It may carry a secret token: shown only to the owner and
    -- never logged.
    source_url         TEXT    NOT NULL,
    -- Refresh interval chosen by the owner; NULL: the feed's
    -- REFRESH-INTERVAL / X-PUBLISHED-TTL, else the server default.
    refresh_secs       INTEGER,
    feed_ttl_secs      INTEGER,
    -- Validators of the last good response, for conditional requests.
    http_etag          TEXT,
    http_last_modified TEXT,
    -- Hash of the last good body: an identical body is not parsed again.
    content_hash       TEXT,
    -- Unix seconds.
    last_attempt_at    INTEGER,
    last_success_at    INTEGER,
    -- Error code of the last failed refresh (NULL after a success) and a
    -- short server-generated detail (never the URL or the response body).
    last_error         TEXT,
    last_error_detail  TEXT,
    -- Consecutive failures, for the back-off.
    failures           INTEGER NOT NULL DEFAULT 0,
    -- Objects of the last good feed that were invalid and left out.
    skipped            INTEGER NOT NULL DEFAULT 0,
    next_refresh_at    INTEGER NOT NULL,
    created_at         INTEGER NOT NULL
) STRICT;

CREATE INDEX subscriptions_due ON subscriptions (next_refresh_at);
