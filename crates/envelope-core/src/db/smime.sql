
-- S/MIME (schema version 12). Private keys live in the system keyring, never
-- here.

-- Certificates: the user's own (their private keys in the keyring), other
-- people's (from their signed mail, or imported), and the authorities
-- between them and a root, kept to complete chains.
CREATE TABLE IF NOT EXISTS smime_certs (
    fingerprint  TEXT PRIMARY KEY,           -- SHA-256 of the certificate, upper-case hex
    data         BLOB NOT NULL,              -- DER
    emails       TEXT NOT NULL DEFAULT ' ',  -- ' a@x b@y ' (lower case): its addresses, and the one it was imported for
    ca           INTEGER NOT NULL DEFAULT 0, -- an authority's certificate
    secret       INTEGER NOT NULL DEFAULT 0, -- the private key is in the keyring
    source       TEXT NOT NULL,              -- own | message | manual | chain
    added_at     INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
