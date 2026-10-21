-- OpenID Connect identities. The first SSO login binds the IdP account
-- (issuer + subject) to the local user; later logins must present the same
-- subject, so a different IdP account with the same username cannot take
-- the account over. A subject belongs to at most one local user.
CREATE TABLE oidc_identities (
    user_id       INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    issuer        TEXT    NOT NULL,
    subject       TEXT    NOT NULL,
    created_at    INTEGER NOT NULL,
    last_login_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, issuer),
    UNIQUE (issuer, subject)
) STRICT;
