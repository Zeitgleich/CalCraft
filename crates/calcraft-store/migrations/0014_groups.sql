-- Groups of users, managed by administrators, and sharing a collection
-- with a group, see `groups.rs` and docs/sharing.md.
--
-- A group share is not an access grant by itself: every member gets their
-- own row in `shares` (an invitation they accept or decline, their own
-- slug, name and colour). `shares.access` stays the one value every access
-- check reads; it is the maximum of the direct share (`direct_access`) and
-- of the group shares of groups the sharee is a member of and did not
-- decline. The store recomputes it inside the same transaction as every
-- change of a direct share, a group share or a membership; a row without
-- any grant left is deleted.

-- Named `user_groups` because GROUPS is an SQL keyword.
CREATE TABLE user_groups (
    id           INTEGER PRIMARY KEY,
    -- Same rules as usernames (lower-case a-z, 0-9, '.', '_', '-').
    name         TEXT    NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 64),
    display_name TEXT,
    -- Unix seconds.
    created_at   INTEGER NOT NULL
) STRICT;

CREATE TABLE group_members (
    group_id INTEGER NOT NULL REFERENCES user_groups (id) ON DELETE CASCADE,
    user_id  INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- Unix seconds.
    added_at INTEGER NOT NULL,
    PRIMARY KEY (group_id, user_id)
) STRICT;

CREATE INDEX group_members_by_user ON group_members (user_id);

-- One collection shared by its owner with a group.
CREATE TABLE group_shares (
    id            INTEGER PRIMARY KEY,
    collection_id INTEGER NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    group_id      INTEGER NOT NULL REFERENCES user_groups (id) ON DELETE CASCADE,
    access        TEXT    NOT NULL CHECK (access IN ('read', 'read-write')),
    -- Unix seconds.
    created_at    INTEGER NOT NULL,
    UNIQUE (collection_id, group_id)
) STRICT;

CREATE INDEX group_shares_by_group ON group_shares (group_id);

-- Members who declined the invitation of a group share, or left the
-- collection: the group share never invites them again (also not after
-- leaving and rejoining the group). Removing the group share forgets it;
-- the owner's 24-hour block in `share_declines` still applies to a new one.
CREATE TABLE group_share_declines (
    group_share_id INTEGER NOT NULL REFERENCES group_shares (id) ON DELETE CASCADE,
    user_id        INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- Unix seconds.
    declined_at    INTEGER NOT NULL,
    PRIMARY KEY (group_share_id, user_id)
) STRICT;

-- The direct (user) share of a row in `shares`; NULL if the row exists
-- only because of group shares. Every existing share is a direct one.
ALTER TABLE shares ADD COLUMN direct_access TEXT
    CHECK (direct_access IS NULL OR direct_access IN ('read', 'read-write'));
UPDATE shares SET direct_access = access;
