-- Groups of users, managed by admins. A group holds users and other groups (nested groups);
-- a user or a group can be in several groups. ("groups" is an SQL keyword: hence user_groups.)
CREATE TABLE user_groups (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
    description TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE user_group_members (
    group_id TEXT NOT NULL REFERENCES user_groups (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (group_id, user_id)
);
CREATE INDEX user_group_members_by_user ON user_group_members (user_id);

-- child_id is a member of group_id. The API refuses cycles (a group inside itself).
CREATE TABLE user_group_subgroups (
    group_id TEXT NOT NULL REFERENCES user_groups (id) ON DELETE CASCADE,
    child_id TEXT NOT NULL REFERENCES user_groups (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (group_id, child_id),
    CHECK (group_id <> child_id)
);
CREATE INDEX user_group_subgroups_by_child ON user_group_subgroups (child_id);
