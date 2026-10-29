-- VIP senders, by lowercase address (as `messages.from_email`).
CREATE TABLE IF NOT EXISTS vips (
    email    TEXT PRIMARY KEY,
    name     TEXT,
    added_at INTEGER NOT NULL
);
