-- Task lists and tasks (schema version 7).

CREATE TABLE IF NOT EXISTS task_lists (
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER REFERENCES accounts(id) ON DELETE CASCADE, -- NULL: on this computer
    kind        TEXT NOT NULL,              -- local | caldav | google | graph | ews
    remote_id   TEXT NOT NULL,              -- CalDAV collection URL, Google or Graph list id, EWS folder id
    name        TEXT NOT NULL,
    color       TEXT NOT NULL DEFAULT '',   -- #rrggbb; empty: the account's color
    read_only   INTEGER NOT NULL DEFAULT 0,
    is_default  INTEGER NOT NULL DEFAULT 0, -- where new tasks of the account go
    sort_order  INTEGER NOT NULL DEFAULT 0,
    sync_state  TEXT NOT NULL DEFAULT '{}',
    UNIQUE (account_id, remote_id)
);

CREATE TABLE IF NOT EXISTS tasks (
    id            INTEGER PRIMARY KEY,
    list_id       INTEGER NOT NULL REFERENCES task_lists(id) ON DELETE CASCADE,
    parent_id     INTEGER REFERENCES tasks(id) ON DELETE SET NULL,
    parent_uid    TEXT,                       -- the parent's uid, as the server links them
    uid           TEXT NOT NULL DEFAULT '',   -- iCalendar UID / the id other tasks name it by
    remote_id     TEXT,                       -- CalDAV href / Google, Graph or EWS id
    etag          TEXT,
    title         TEXT NOT NULL DEFAULT '',
    notes         TEXT NOT NULL DEFAULT '',
    due_date      TEXT,                       -- YYYY-MM-DD, local
    due_at        INTEGER,                    -- due at a time of day; due_date is then its local day
    completed_at  INTEGER,
    important     INTEGER NOT NULL DEFAULT 0,
    position      INTEGER NOT NULL DEFAULT 0, -- order among the tasks with the same parent
    source        TEXT,                       -- JSON TaskSource: the message the task was made from
    data          TEXT,                       -- the server copy (iCalendar, JSON) local changes started from
    repeats       INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    notified      INTEGER NOT NULL DEFAULT 0, -- the due notification was shown
    local_state   INTEGER NOT NULL DEFAULT 0, -- 0 synced 1 created 2 modified 3 deleted
    version       INTEGER NOT NULL DEFAULT 0, -- bumped by every local change
    push_attempts INTEGER NOT NULL DEFAULT 0,
    push_after    INTEGER NOT NULL DEFAULT 0,
    push_error    TEXT,
    deleted_state INTEGER,                    -- deleted by task_delete: the local_state undoing it brings back
    deleted_with  INTEGER,                    -- ...and the task whose deletion took it along (itself, or its parent)
    sent_at       INTEGER                     -- a create whose answer got lost, when: it may be on the server
);
CREATE INDEX IF NOT EXISTS tasks_list ON tasks(list_id, parent_id, position);
CREATE INDEX IF NOT EXISTS tasks_parent ON tasks(parent_id) WHERE parent_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS tasks_remote ON tasks(list_id, remote_id);
CREATE INDEX IF NOT EXISTS tasks_due ON tasks(due_date) WHERE completed_at IS NULL;
CREATE INDEX IF NOT EXISTS tasks_notify ON tasks(due_at) WHERE notified = 0 AND completed_at IS NULL;
CREATE INDEX IF NOT EXISTS tasks_dirty ON tasks(list_id) WHERE local_state != 0;
