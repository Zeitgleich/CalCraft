-- Readiness probe (`/readyz`): proves the database can be written without
-- touching user data or growing the file.
--
-- The probe updates `probe` of the single row in place inside a
-- transaction it rolls back. Only after a write failed for lack of space
-- does it commit `pad` (a 64 KiB blob that needs new pages) and set it back
-- to NULL right after; the freed pages are reused by later writes.
CREATE TABLE health (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    probe INTEGER NOT NULL DEFAULT 0,
    pad BLOB
);

INSERT INTO health (id) VALUES (1);
