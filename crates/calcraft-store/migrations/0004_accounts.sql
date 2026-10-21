ALTER TABLE users ADD COLUMN is_admin INTEGER NOT NULL DEFAULT 0 CHECK (is_admin IN (0, 1));
-- TOTP secret (RFC 6238); NULL while 2FA is not set up.
ALTER TABLE users ADD COLUMN totp_secret BLOB;
ALTER TABLE users ADD COLUMN totp_enabled INTEGER NOT NULL DEFAULT 0 CHECK (totp_enabled IN (0, 1));
-- Last accepted TOTP time step; codes for this or earlier steps are rejected (no replay).
ALTER TABLE users ADD COLUMN totp_last_step INTEGER NOT NULL DEFAULT 0;

-- Web UI sessions. Only a hash of the cookie value is stored.
CREATE TABLE sessions (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    token_hash TEXT    NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;
CREATE INDEX sessions_by_user ON sessions (user_id);

-- Per-device passwords for DAV clients. The secret is random (160 bits),
-- so a fast keyed hash is sufficient; only the hash is stored.
CREATE TABLE app_passwords (
    id           INTEGER PRIMARY KEY,
    user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name         TEXT    NOT NULL,
    secret_hash  TEXT    NOT NULL,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER
) STRICT;
CREATE INDEX app_passwords_by_user ON app_passwords (user_id);

-- One-time recovery codes for 2FA, stored hashed.
CREATE TABLE recovery_codes (
    user_id   INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    code_hash TEXT    NOT NULL,
    PRIMARY KEY (user_id, code_hash)
) STRICT;
