-- Envelope local store. Schema version 1.
-- Times are unix milliseconds (UTC) unless noted otherwise.

CREATE TABLE IF NOT EXISTS kv (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS accounts (
    id          INTEGER PRIMARY KEY,
    kind        TEXT NOT NULL,              -- imap | gmail | microsoft
    provider    TEXT NOT NULL,              -- preset id (gmail, outlook, icloud, yahoo, fastmail, generic, ...)
    email       TEXT NOT NULL,
    name        TEXT NOT NULL,              -- the user's display name
    label       TEXT,                       -- sidebar label override
    color       TEXT NOT NULL,
    config      TEXT NOT NULL,              -- JSON AccountConfig (servers, auth)
    state       TEXT NOT NULL DEFAULT '{}', -- JSON runtime state (capabilities, last sync)
    sort_order  INTEGER NOT NULL DEFAULT 0,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS identities (
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    email       TEXT NOT NULL,
    reply_to    TEXT,
    signature   TEXT,                       -- HTML
    is_default  INTEGER NOT NULL DEFAULT 0,
    sort_order  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS identities_account ON identities(account_id);

CREATE TABLE IF NOT EXISTS folders (
    id            INTEGER PRIMARY KEY,
    account_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    remote_id     TEXT NOT NULL,            -- IMAP path, Graph folder id, Gmail label
    name          TEXT NOT NULL,            -- last path component
    path          TEXT NOT NULL,            -- full display path
    parent_id     INTEGER,
    role          TEXT,                     -- inbox sent drafts trash junk archive all starred important
    kind          TEXT NOT NULL DEFAULT 'folder', -- folder | label
    delimiter     TEXT,
    selectable    INTEGER NOT NULL DEFAULT 1,
    hidden        INTEGER NOT NULL DEFAULT 0,
    color         TEXT,
    sort_order    INTEGER NOT NULL DEFAULT 0,
    unread_count  INTEGER NOT NULL DEFAULT 0,  -- unread threads
    total_count   INTEGER NOT NULL DEFAULT 0,  -- threads
    sync_state    TEXT NOT NULL DEFAULT '{}',
    UNIQUE (account_id, remote_id)
);
CREATE INDEX IF NOT EXISTS folders_role ON folders(role);

CREATE TABLE IF NOT EXISTS threads (
    id               INTEGER PRIMARY KEY,
    account_id       INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    remote_thread_id TEXT,
    subject          TEXT NOT NULL DEFAULT '',
    snippet          TEXT NOT NULL DEFAULT '',
    last_date        INTEGER NOT NULL DEFAULT 0,
    message_count    INTEGER NOT NULL DEFAULT 0,
    unread_count     INTEGER NOT NULL DEFAULT 0,
    flagged          INTEGER NOT NULL DEFAULT 0,
    has_attachments  INTEGER NOT NULL DEFAULT 0,
    draft_count      INTEGER NOT NULL DEFAULT 0,
    participants     TEXT NOT NULL DEFAULT '[]', -- JSON [{name,email}] of senders, oldest first
    last_sender      TEXT,                       -- JSON {name,email} of the newest message's sender
    category         TEXT,                       -- people | newsletters | notifications
    is_invite        INTEGER NOT NULL DEFAULT 0,
    muted            INTEGER NOT NULL DEFAULT 0,  -- later replies skip the inbox
    snoozed_until    INTEGER,
    sender           TEXT                        -- sort key of whom it is with (db::mail::sort_key)
);
CREATE INDEX IF NOT EXISTS threads_remote ON threads(account_id, remote_thread_id);
CREATE INDEX IF NOT EXISTS threads_snoozed ON threads(snoozed_until) WHERE snoozed_until IS NOT NULL;

-- Which folders/labels a thread appears in; drives every message list query.
CREATE TABLE IF NOT EXISTS thread_folders (
    folder_id  INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    thread_id  INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    last_date  INTEGER NOT NULL,
    unread     INTEGER NOT NULL DEFAULT 0,
    sender     TEXT,                        -- the thread's, for lists by sender
    PRIMARY KEY (folder_id, thread_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS thread_folders_date ON thread_folders(folder_id, last_date DESC);
CREATE INDEX IF NOT EXISTS thread_folders_unread ON thread_folders(folder_id, last_date DESC) WHERE unread > 0;
CREATE INDEX IF NOT EXISTS thread_folders_thread ON thread_folders(thread_id);

CREATE TABLE IF NOT EXISTS messages (
    id                INTEGER PRIMARY KEY,
    account_id        INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    thread_id         INTEGER NOT NULL,
    remote_id         TEXT,                 -- Graph id / X-GM-MSGID
    message_id        TEXT,                 -- RFC 5322 Message-ID without brackets
    in_reply_to       TEXT,
    refs              TEXT,                 -- space separated
    subject           TEXT NOT NULL DEFAULT '',
    from_name         TEXT,
    from_email        TEXT,
    to_json           TEXT NOT NULL DEFAULT '[]',
    cc_json           TEXT NOT NULL DEFAULT '[]',
    bcc_json          TEXT NOT NULL DEFAULT '[]',
    reply_to_json     TEXT NOT NULL DEFAULT '[]',
    date              INTEGER NOT NULL,
    snippet           TEXT NOT NULL DEFAULT '',
    flags             INTEGER NOT NULL DEFAULT 0, -- 1 seen 2 flagged 4 answered 8 draft 16 forwarded
    keywords          TEXT,
    size              INTEGER NOT NULL DEFAULT 0,
    has_attachments   INTEGER NOT NULL DEFAULT 0,
    structure         TEXT,                 -- JSON body structure (IMAP part ids)
    body_state        INTEGER NOT NULL DEFAULT 0, -- 0 missing, 1 present, 2 failed
    list_unsubscribe  TEXT,
    unsubscribe_post  INTEGER NOT NULL DEFAULT 0,
    list_id           TEXT,
    is_auto           INTEGER NOT NULL DEFAULT 0,
    calendar_method   TEXT                  -- iTIP METHOD when the message carries an invitation
);
CREATE INDEX IF NOT EXISTS messages_thread ON messages(thread_id, date);
CREATE INDEX IF NOT EXISTS messages_msgid ON messages(account_id, message_id);
CREATE INDEX IF NOT EXISTS messages_remote ON messages(account_id, remote_id);
CREATE INDEX IF NOT EXISTS messages_body ON messages(account_id, body_state, date DESC);
CREATE INDEX IF NOT EXISTS messages_from ON messages(from_email);
CREATE INDEX IF NOT EXISTS messages_reply ON messages(account_id, in_reply_to) WHERE in_reply_to IS NOT NULL;

-- Server locations of a message: one row per IMAP folder copy or Gmail label.
-- Date and sender are the message's (given whenever a message is put in a
-- folder: see db::mail::add_location), for lists of messages one by one.
CREATE TABLE IF NOT EXISTS message_folders (
    folder_id   INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    message_id  INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    uid         INTEGER,
    date        INTEGER,
    sender      TEXT,
    PRIMARY KEY (folder_id, message_id)
) WITHOUT ROWID;
CREATE UNIQUE INDEX IF NOT EXISTS message_folders_uid ON message_folders(folder_id, uid) WHERE uid IS NOT NULL;
CREATE INDEX IF NOT EXISTS message_folders_message ON message_folders(message_id);

CREATE TABLE IF NOT EXISTS bodies (
    message_id  INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    html        BLOB,                       -- zstd compressed
    text        BLOB,                       -- zstd compressed
    fetched_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS attachments (
    id          INTEGER PRIMARY KEY,
    message_id  INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    part_id     TEXT NOT NULL,              -- IMAP part specifier or MIME part index path
    filename    TEXT NOT NULL,
    mime_type   TEXT NOT NULL,
    size        INTEGER NOT NULL DEFAULT 0,
    content_id  TEXT,
    inline      INTEGER NOT NULL DEFAULT 0,
    encoding    TEXT
);
CREATE INDEX IF NOT EXISTS attachments_message ON attachments(message_id);

CREATE VIRTUAL TABLE IF NOT EXISTS message_fts USING fts5(
    subject, sender, recipients, body, attachments,
    content='', contentless_delete=1,
    tokenize='unicode61 remove_diacritics 2',
    prefix='2 3'
);

-- Address book for autocomplete: every address we have seen or written to.
CREATE TABLE IF NOT EXISTS addresses (
    email          TEXT PRIMARY KEY COLLATE NOCASE,
    name           TEXT,
    sent_count     INTEGER NOT NULL DEFAULT 0,
    received_count INTEGER NOT NULL DEFAULT 0,
    last_used      INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;

-- Address books and contacts live in contacts.sql.

-- Local changes waiting to be replayed against the server.
CREATE TABLE IF NOT EXISTS pending_ops (
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    payload     TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    not_before  INTEGER NOT NULL DEFAULT 0,
    attempts    INTEGER NOT NULL DEFAULT 0,
    last_error  TEXT
);
CREATE INDEX IF NOT EXISTS pending_ops_account ON pending_ops(account_id, id);

-- Messages waiting to be sent (undo send window, scheduled send, offline).
CREATE TABLE IF NOT EXISTS outbox (
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    draft_id    INTEGER,
    mime        BLOB NOT NULL,
    meta        TEXT NOT NULL,              -- JSON OutboxMeta
    send_at     INTEGER NOT NULL,
    state       TEXT NOT NULL DEFAULT 'queued', -- queued | sending | failed
    attempts    INTEGER NOT NULL DEFAULT 0,
    error       TEXT,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS drafts (
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    data        TEXT NOT NULL,              -- JSON DraftInput
    remote_uid  INTEGER,                    -- server copy in the Drafts folder (IMAP)
    remote_id   TEXT,                       -- server copy id (Graph)
    message_id  TEXT,                       -- Message-ID of the server copy, no brackets
    synced_at   INTEGER,                    -- updated_at of the version the server copy holds
    updated_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS calendars (
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER REFERENCES accounts(id) ON DELETE CASCADE, -- NULL: local or subscription
    kind        TEXT NOT NULL,              -- caldav | google | graph | local | subscription
    remote_id   TEXT NOT NULL,
    name        TEXT NOT NULL,
    color       TEXT NOT NULL,
    description TEXT,
    timezone    TEXT,
    read_only   INTEGER NOT NULL DEFAULT 0,
    is_default  INTEGER NOT NULL DEFAULT 0,
    visible     INTEGER NOT NULL DEFAULT 1,
    sort_order  INTEGER NOT NULL DEFAULT 0,
    sync_state  TEXT NOT NULL DEFAULT '{}',
    UNIQUE (account_id, remote_id)
);

CREATE TABLE IF NOT EXISTS events (
    id             INTEGER PRIMARY KEY,
    calendar_id    INTEGER NOT NULL REFERENCES calendars(id) ON DELETE CASCADE,
    remote_id      TEXT,                    -- CalDAV href / Google id / Graph id
    etag           TEXT,
    uid            TEXT NOT NULL,
    recurrence_id  TEXT,                    -- JSON EventTime of an overridden occurrence
    start_ts       INTEGER NOT NULL,
    end_ts         INTEGER NOT NULL,
    until_ts       INTEGER,                 -- recurring masters: last possible end (NULL = forever)
    all_day        INTEGER NOT NULL DEFAULT 0,
    is_recurring   INTEGER NOT NULL DEFAULT 0,
    summary        TEXT NOT NULL DEFAULT '',
    data           TEXT NOT NULL,           -- JSON envelope_ical::Event
    raw            TEXT,                    -- source iCalendar text (CalDAV) for lossless edits
    provider_data  TEXT,                    -- JSON provider specifics
    local_state    INTEGER NOT NULL DEFAULT 0 -- 0 synced 1 created 2 modified 3 deleted
);
CREATE INDEX IF NOT EXISTS events_range ON events(calendar_id, start_ts);
CREATE INDEX IF NOT EXISTS events_uid ON events(calendar_id, uid);
CREATE INDEX IF NOT EXISTS events_remote ON events(calendar_id, remote_id);

-- "Remind me if no reply": one row per sent message being watched.
CREATE TABLE IF NOT EXISTS followups (
    message_id  TEXT PRIMARY KEY,          -- Message-ID of the sent message, no brackets
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    subject     TEXT NOT NULL DEFAULT '',
    since       INTEGER NOT NULL,          -- when it was sent (unix ms)
    due_at      INTEGER NOT NULL,
    notified    INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS followups_due ON followups(notified, due_at);
