-- Address books and contacts (schema version 3).

CREATE TABLE IF NOT EXISTS address_books (
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER REFERENCES accounts(id) ON DELETE CASCADE, -- NULL: on this computer
    kind        TEXT NOT NULL,              -- carddav | google | graph | local
    remote_id   TEXT NOT NULL,              -- CardDAV collection URL, Graph folder id, ...
    name        TEXT NOT NULL,
    read_only   INTEGER NOT NULL DEFAULT 0,
    is_default  INTEGER NOT NULL DEFAULT 0,
    sort_order  INTEGER NOT NULL DEFAULT 0,
    sync_state  TEXT NOT NULL DEFAULT '{}',
    UNIQUE (account_id, remote_id)
);

CREATE TABLE IF NOT EXISTS contacts (
    id            INTEGER PRIMARY KEY,
    book_id       INTEGER NOT NULL REFERENCES address_books(id) ON DELETE CASCADE,
    uid           TEXT NOT NULL DEFAULT '',
    remote_id     TEXT,                       -- CardDAV href / Google resource name / Graph id
    etag          TEXT,
    name          TEXT NOT NULL DEFAULT '',
    sort_key      TEXT NOT NULL DEFAULT '',   -- list order: section letter, then the folded name
    search        TEXT NOT NULL DEFAULT '',   -- folded names, organization, e-mails, phone digits
    email         TEXT,                       -- primary e-mail address
    emails        TEXT NOT NULL DEFAULT '',   -- every e-mail address, one per line
    organization  TEXT,
    data          TEXT NOT NULL,              -- JSON envelope_ical::Contact (photo kept below)
    base          BLOB,                       -- zstd: the server copy local changes started from
    is_group      INTEGER NOT NULL DEFAULT 0, -- a contact group card, not a person
    photo         BLOB,
    photo_mime    TEXT,
    photo_hash    TEXT,
    photo_url     TEXT,                       -- remote photo to fetch
    photo_src     TEXT,                       -- remote photo the stored one came from
    photo_dirty   INTEGER NOT NULL DEFAULT 0, -- photo changed here, not yet on the server
    thumb         TEXT,                       -- small data: URL for lists
    local_state   INTEGER NOT NULL DEFAULT 0, -- 0 synced 1 created 2 modified 3 deleted
    version       INTEGER NOT NULL DEFAULT 0, -- bumped by every local change
    push_attempts INTEGER NOT NULL DEFAULT 0,
    push_after    INTEGER NOT NULL DEFAULT 0,
    push_error    TEXT
);
CREATE INDEX IF NOT EXISTS contacts_order ON contacts(sort_key);
CREATE INDEX IF NOT EXISTS contacts_book ON contacts(book_id, sort_key);
CREATE INDEX IF NOT EXISTS contacts_remote ON contacts(book_id, remote_id);
CREATE INDEX IF NOT EXISTS contacts_dirty ON contacts(book_id) WHERE local_state != 0;
