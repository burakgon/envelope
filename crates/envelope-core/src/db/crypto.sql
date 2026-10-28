
-- OpenPGP (schema version 5). Secret keys live in the system keyring, never
-- here.

-- Certificates: the user's own keys and other people's public keys.
CREATE TABLE IF NOT EXISTS pgp_keys (
    fingerprint  TEXT PRIMARY KEY,            -- upper-case hex of the primary key
    data         BLOB NOT NULL,               -- binary transferable public key
    user_ids     TEXT NOT NULL DEFAULT '[]',  -- JSON array
    emails       TEXT NOT NULL DEFAULT ' ',   -- ' a@x b@y ' (lower case), for lookups
    key_ids      TEXT NOT NULL DEFAULT ' ',   -- ' HEX16 HEX16 ' of the primary key and subkeys
    algorithm    TEXT NOT NULL DEFAULT '',
    created_at   INTEGER NOT NULL DEFAULT 0,  -- unix ms
    expires_at   INTEGER,                     -- unix ms; NULL: never
    revoked      INTEGER NOT NULL DEFAULT 0,
    can_encrypt  INTEGER NOT NULL DEFAULT 0,
    can_sign     INTEGER NOT NULL DEFAULT 0,
    secret       INTEGER NOT NULL DEFAULT 0,  -- the secret key is in the keyring
    protected    INTEGER NOT NULL DEFAULT 0,  -- the secret key needs a passphrase
    source       TEXT NOT NULL,               -- own | autocrypt | gossip | wkd | keyserver | manual
    verified     INTEGER NOT NULL DEFAULT 0,  -- the user compared the fingerprint
    added_at     INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);

-- Autocrypt peer state per address (https://autocrypt.org/level1.html).
CREATE TABLE IF NOT EXISTS pgp_peers (
    email               TEXT PRIMARY KEY COLLATE NOCASE,
    last_seen           INTEGER NOT NULL DEFAULT 0,  -- newest message from the address (unix ms)
    autocrypt_at        INTEGER NOT NULL DEFAULT 0,  -- date of the message the key came with
    fingerprint         TEXT,                        -- key of the newest Autocrypt header
    prefer_encrypt      INTEGER NOT NULL DEFAULT 0,  -- 1: prefer-encrypt=mutual
    gossip_at           INTEGER NOT NULL DEFAULT 0,
    gossip_fingerprint  TEXT                         -- key from Autocrypt-Gossip
) WITHOUT ROWID;

-- The user's own key for each address they send from.
CREATE TABLE IF NOT EXISTS pgp_own (
    email        TEXT PRIMARY KEY COLLATE NOCASE,
    fingerprint  TEXT NOT NULL
) WITHOUT ROWID;

-- Messages with OpenPGP content and what is needed to decrypt or verify them.
CREATE TABLE IF NOT EXISTS message_crypto (
    message_id  INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,              -- encrypted | signed | inline-encrypted | inline-signed
    raw         BLOB,                       -- zstd compressed source; NULL when too large
    decrypted   INTEGER NOT NULL DEFAULT 0  -- a decrypted copy is kept in bodies
);
