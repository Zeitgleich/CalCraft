-- E-mail reminders for alarms (VALARM) and the outgoing mail queue, see
-- `reminders.rs` and docs/reminders.md.

-- Which alarms send an e-mail to the user (set by the user): 'off',
-- 'email' (ACTION:EMAIL only) or 'all' (also DISPLAY and AUDIO). NULL: the
-- server default (`reminders.default_mode`).
ALTER TABLE users ADD COLUMN reminder_mode TEXT CHECK (reminder_mode IS NULL OR reminder_mode IN ('off', 'email', 'all'));
-- Whether reminder mails include the event's description (off by default:
-- descriptions often hold more than a mail should carry).
ALTER TABLE users ADD COLUMN reminder_description INTEGER NOT NULL DEFAULT 0 CHECK (reminder_description IN (0, 1));

-- The server default mode (`reminders.default_mode`) the index was built
-- with; when it changes, every calendar is indexed again.
CREATE TABLE reminder_state (
    id           INTEGER PRIMARY KEY CHECK (id = 1),
    default_mode TEXT    NOT NULL CHECK (default_mode IN ('off', 'email', 'all'))
) STRICT;

-- Per calendar: the collection revision (`collections.sync_revision`) up to
-- which its objects are in `reminder_index`. Objects changed later (PUT,
-- MOVE, COPY, restore, scheduling deliveries, imports) have a higher
-- `items.revision` and are indexed again; deleted objects leave the index
-- through the foreign key.
CREATE TABLE reminder_cursors (
    collection_id INTEGER PRIMARY KEY REFERENCES collections (id) ON DELETE CASCADE,
    revision      INTEGER NOT NULL
) STRICT;

-- The next moment an object's alarms need attention.
CREATE TABLE reminder_index (
    item_id   INTEGER PRIMARY KEY REFERENCES items (id) ON DELETE CASCADE,
    -- ETag of the content `next_due` was computed from.
    etag      TEXT    NOT NULL,
    -- Alarms triggering at or before this moment are done (Unix seconds):
    -- sent, queued, or too late. Only ever moves forward.
    scan_from INTEGER NOT NULL,
    -- Next trigger, or a later re-check of objects with alarms beyond the
    -- search horizon; NULL: nothing to do.
    next_due  INTEGER
) STRICT;

CREATE INDEX reminder_index_due ON reminder_index (next_due) WHERE next_due IS NOT NULL;

-- Outgoing e-mail (reminders, test mails; later iMIP). A message is queued
-- in the same transaction that marks its alarm as done, so a restart can
-- neither lose nor repeat it; the sender delivers it with retries. Recipient
-- and message are removed once the mail is sent or given up; the row stays
-- a few days for the per-user limits and the status shown to the user.
CREATE TABLE mail_outbox (
    id              INTEGER PRIMARY KEY,
    user_id         INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind            TEXT    NOT NULL CHECK (kind IN ('reminder', 'test')),
    -- The object a reminder is about; NULL once it is deleted (a pending
    -- reminder is then dropped).
    item_id         INTEGER REFERENCES items (id) ON DELETE SET NULL,
    -- Identity of a reminder (calendar, UID, instance, trigger): the same
    -- reminder is never queued twice.
    dedupe_key      TEXT    UNIQUE,
    -- The user's own address at the time of queueing.
    recipient       TEXT,
    -- Complete RFC 5322 message with CRLF line ends.
    message         BLOB,
    status          TEXT    NOT NULL CHECK (status IN ('pending', 'sent', 'failed', 'dropped', 'cancelled')),
    -- Short machine-readable reason of the last failure (never server
    -- replies, addresses or content).
    error           TEXT,
    attempts        INTEGER NOT NULL DEFAULT 0,
    -- Unix seconds.
    next_attempt_at INTEGER NOT NULL,
    expires_at      INTEGER NOT NULL,
    created_at      INTEGER NOT NULL,
    done_at         INTEGER,
    CHECK ((status = 'pending') = (message IS NOT NULL))
) STRICT;

CREATE INDEX mail_outbox_pending ON mail_outbox (next_attempt_at, id) WHERE status = 'pending';
CREATE INDEX mail_outbox_by_user ON mail_outbox (user_id, created_at);
CREATE INDEX mail_outbox_by_age ON mail_outbox (created_at);
