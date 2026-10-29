//! OpenPGP keys, Autocrypt peer state and the OpenPGP data of messages.

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use crate::error::Result;

/// A certificate as stored (`pgp_keys`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeyRow {
    pub fingerprint: String,
    /// Binary transferable public key.
    pub data: Vec<u8>,
    pub user_ids: Vec<String>,
    pub emails: Vec<String>,
    pub key_ids: Vec<String>,
    pub algorithm: String,
    /// Unix ms.
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub revoked: bool,
    pub can_encrypt: bool,
    pub can_sign: bool,
    pub secret: bool,
    pub protected: bool,
    pub source: String,
    pub verified: bool,
    pub added_at: i64,
    pub updated_at: i64,
}

/// A list as stored for `instr` lookups: ` a b c `.
pub(crate) fn spaced(list: &[String]) -> String {
    if list.is_empty() {
        " ".into()
    } else {
        format!(" {} ", list.join(" "))
    }
}

pub(crate) fn unspaced(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

const KEY_COLUMNS: &str =
    "fingerprint, data, user_ids, emails, key_ids, algorithm, created_at, expires_at, revoked,
     can_encrypt, can_sign, secret, protected, source, verified, added_at, updated_at";

fn key_from_row(r: &Row<'_>) -> rusqlite::Result<KeyRow> {
    let user_ids: String = r.get(2)?;
    Ok(KeyRow {
        fingerprint: r.get(0)?,
        data: r.get(1)?,
        user_ids: serde_json::from_str(&user_ids).unwrap_or_default(),
        emails: unspaced(&r.get::<_, String>(3)?),
        key_ids: unspaced(&r.get::<_, String>(4)?),
        algorithm: r.get(5)?,
        created_at: r.get(6)?,
        expires_at: r.get(7)?,
        revoked: r.get::<_, i64>(8)? != 0,
        can_encrypt: r.get::<_, i64>(9)? != 0,
        can_sign: r.get::<_, i64>(10)? != 0,
        secret: r.get::<_, i64>(11)? != 0,
        protected: r.get::<_, i64>(12)? != 0,
        source: r.get(13)?,
        verified: r.get::<_, i64>(14)? != 0,
        added_at: r.get(15)?,
        updated_at: r.get(16)?,
    })
}

/// Inserts or replaces a certificate.
pub fn key_save(tx: &Transaction<'_>, k: &KeyRow) -> Result<()> {
    tx.execute(
        &format!(
            "INSERT INTO pgp_keys ({KEY_COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (fingerprint) DO UPDATE SET data = excluded.data, user_ids = excluded.user_ids,
               emails = excluded.emails, key_ids = excluded.key_ids, algorithm = excluded.algorithm,
               created_at = excluded.created_at, expires_at = excluded.expires_at, revoked = excluded.revoked,
               can_encrypt = excluded.can_encrypt, can_sign = excluded.can_sign, secret = excluded.secret,
               protected = excluded.protected, source = excluded.source, verified = excluded.verified,
               updated_at = excluded.updated_at"
        ),
        params![
            k.fingerprint,
            k.data,
            serde_json::to_string(&k.user_ids)?,
            spaced(&k.emails),
            spaced(&k.key_ids),
            k.algorithm,
            k.created_at,
            k.expires_at,
            k.revoked as i64,
            k.can_encrypt as i64,
            k.can_sign as i64,
            k.secret as i64,
            k.protected as i64,
            k.source,
            k.verified as i64,
            k.added_at,
            k.updated_at,
        ],
    )?;
    Ok(())
}

pub fn key_get(c: &Connection, fingerprint: &str) -> Result<Option<KeyRow>> {
    Ok(c.query_row(
        &format!("SELECT {KEY_COLUMNS} FROM pgp_keys WHERE fingerprint = ?"),
        [fingerprint],
        key_from_row,
    )
    .optional()?)
}

/// Deletes a certificate and every reference to it.
pub fn key_delete(tx: &Transaction<'_>, fingerprint: &str) -> Result<()> {
    tx.execute("DELETE FROM pgp_keys WHERE fingerprint = ?", [fingerprint])?;
    tx.execute("DELETE FROM pgp_own WHERE fingerprint = ?", [fingerprint])?;
    tx.execute(
        "UPDATE pgp_peers SET fingerprint = NULL WHERE fingerprint = ?",
        [fingerprint],
    )?;
    tx.execute(
        "UPDATE pgp_peers SET gossip_fingerprint = NULL WHERE gossip_fingerprint = ?",
        [fingerprint],
    )?;
    Ok(())
}

pub fn key_set_verified(tx: &Transaction<'_>, fingerprint: &str, verified: bool) -> Result<bool> {
    Ok(tx.execute(
        "UPDATE pgp_keys SET verified = ? WHERE fingerprint = ?",
        params![verified as i64, fingerprint],
    )? > 0)
}

/// All certificates matching `query` (address, name or fingerprint), the
/// user's own first, then by name.
pub fn keys_list(c: &Connection, query: &str) -> Result<Vec<KeyRow>> {
    let q = query.trim().to_lowercase();
    let pattern = format!("%{}%", q.replace(' ', ""));
    let mut st = c.prepare(&format!(
        "SELECT {KEY_COLUMNS} FROM pgp_keys
         WHERE ? = '' OR emails LIKE ? OR lower(user_ids) LIKE ? OR lower(fingerprint) LIKE ?
         ORDER BY secret DESC, lower(user_ids), fingerprint"
    ))?;
    let words = format!("%{q}%");
    let rows = st.query_map(params![q, words, words, pattern], key_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Certificates containing any of the given key ids (upper-case hex).
pub fn keys_by_key_ids(c: &Connection, ids: &[String]) -> Result<Vec<KeyRow>> {
    let mut out: Vec<KeyRow> = Vec::new();
    let mut st = c.prepare_cached(&format!(
        "SELECT {KEY_COLUMNS} FROM pgp_keys WHERE instr(key_ids, ' ' || ? || ' ') > 0"
    ))?;
    for id in ids {
        for row in st.query_map([id.to_uppercase()], key_from_row)? {
            let row = row?;
            if !out.iter().any(|k| k.fingerprint == row.fingerprint) {
                out.push(row);
            }
        }
    }
    Ok(out)
}

/// Certificates for an address (from their user ids or Autocrypt).
pub fn keys_for_email(c: &Connection, email: &str) -> Result<Vec<KeyRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {KEY_COLUMNS} FROM pgp_keys WHERE instr(emails, ' ' || ? || ' ') > 0"
    ))?;
    let rows = st.query_map([email.trim().to_lowercase()], key_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Certificates whose secret key is in the keyring.
pub fn secret_keys(c: &Connection) -> Result<Vec<KeyRow>> {
    let mut st = c.prepare(&format!(
        "SELECT {KEY_COLUMNS} FROM pgp_keys WHERE secret = 1"
    ))?;
    let rows = st.query_map([], key_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// ---------------------------------------------------------------------------
// Autocrypt peers

/// Autocrypt peer state of one address.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Peer {
    pub email: String,
    pub last_seen: i64,
    pub autocrypt_at: i64,
    pub fingerprint: Option<String>,
    pub prefer_encrypt: bool,
    pub gossip_at: i64,
    pub gossip_fingerprint: Option<String>,
}

pub fn peer_get(c: &Connection, email: &str) -> Result<Option<Peer>> {
    Ok(c.query_row(
        "SELECT email, last_seen, autocrypt_at, fingerprint, prefer_encrypt, gossip_at, gossip_fingerprint
         FROM pgp_peers WHERE email = ?",
        [email.trim()],
        |r| {
            Ok(Peer {
                email: r.get(0)?,
                last_seen: r.get(1)?,
                autocrypt_at: r.get(2)?,
                fingerprint: r.get(3)?,
                prefer_encrypt: r.get::<_, i64>(4)? != 0,
                gossip_at: r.get(5)?,
                gossip_fingerprint: r.get(6)?,
            })
        },
    )
    .optional()?)
}

pub fn peer_save(tx: &Transaction<'_>, p: &Peer) -> Result<()> {
    tx.execute(
        "INSERT INTO pgp_peers (email, last_seen, autocrypt_at, fingerprint, prefer_encrypt, gossip_at, gossip_fingerprint)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (email) DO UPDATE SET last_seen = excluded.last_seen, autocrypt_at = excluded.autocrypt_at,
           fingerprint = excluded.fingerprint, prefer_encrypt = excluded.prefer_encrypt,
           gossip_at = excluded.gossip_at, gossip_fingerprint = excluded.gossip_fingerprint",
        params![
            p.email.trim().to_lowercase(),
            p.last_seen,
            p.autocrypt_at,
            p.fingerprint,
            p.prefer_encrypt as i64,
            p.gossip_at,
            p.gossip_fingerprint
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The user's own keys

pub fn own_get(c: &Connection, email: &str) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT fingerprint FROM pgp_own WHERE email = ?",
        [email.trim()],
        |r| r.get(0),
    )
    .optional()?)
}

pub fn own_set(tx: &Transaction<'_>, email: &str, fingerprint: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO pgp_own (email, fingerprint) VALUES (?, ?)
         ON CONFLICT (email) DO UPDATE SET fingerprint = excluded.fingerprint",
        params![email.trim().to_lowercase(), fingerprint],
    )?;
    Ok(())
}

pub fn own_all(c: &Connection) -> Result<Vec<(String, String)>> {
    let mut st = c.prepare("SELECT email, fingerprint FROM pgp_own ORDER BY email")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// ---------------------------------------------------------------------------
// OpenPGP content of messages

/// Sources larger than this are not kept; they are downloaded again when the
/// message is opened.
pub const MAX_KEPT_SOURCE: usize = 25 * 1024 * 1024;

/// What is stored about a message with OpenPGP content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageCrypto {
    pub kind: String,
    pub raw: Option<Vec<u8>>,
    /// A decrypted copy is in `bodies`.
    pub decrypted: bool,
}

pub fn message_crypto_set(
    tx: &Transaction<'_>,
    message_id: i64,
    kind: &str,
    raw: &[u8],
) -> Result<()> {
    let packed = (raw.len() <= MAX_KEPT_SOURCE)
        .then(|| zstd::bulk::compress(raw, 3).unwrap_or_else(|_| raw.to_vec()));
    tx.execute(
        "INSERT INTO message_crypto (message_id, kind, raw, decrypted) VALUES (?, ?, ?, 0)
         ON CONFLICT (message_id) DO UPDATE SET kind = excluded.kind, raw = excluded.raw",
        params![message_id, kind, packed],
    )?;
    Ok(())
}

pub fn message_crypto_get(c: &Connection, message_id: i64) -> Result<Option<MessageCrypto>> {
    let row: Option<(String, Option<Vec<u8>>, i64)> = c
        .query_row(
            "SELECT kind, raw, decrypted FROM message_crypto WHERE message_id = ?",
            [message_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(row.map(|(kind, raw, decrypted)| MessageCrypto {
        kind,
        raw: raw.map(|data| unpack_bytes(&data)),
        decrypted: decrypted != 0,
    }))
}

/// The kind of OpenPGP content of a message, without loading its source.
pub fn message_crypto_kind(c: &Connection, message_id: i64) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT kind FROM message_crypto WHERE message_id = ?",
        [message_id],
        |r| r.get(0),
    )
    .optional()?)
}

pub fn message_crypto_delete(tx: &Transaction<'_>, message_id: i64) -> Result<()> {
    tx.execute(
        "DELETE FROM message_crypto WHERE message_id = ?",
        [message_id],
    )?;
    Ok(())
}

pub fn message_crypto_set_decrypted(
    tx: &Transaction<'_>,
    message_id: i64,
    decrypted: bool,
) -> Result<()> {
    tx.execute(
        "UPDATE message_crypto SET decrypted = ? WHERE message_id = ?",
        params![decrypted as i64, message_id],
    )?;
    Ok(())
}

/// Messages whose decrypted copies are kept (to forget them all).
pub fn decrypted_messages(c: &Connection) -> Result<Vec<i64>> {
    let mut st = c.prepare("SELECT message_id FROM message_crypto WHERE decrypted = 1")?;
    let rows = st.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn unpack_bytes(data: &[u8]) -> Vec<u8> {
    if data.len() >= 4
        && data[..4] == [0x28, 0xB5, 0x2F, 0xFD]
        && let Ok(raw) = zstd::stream::decode_all(data)
    {
        return raw;
    }
    data.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn key(fpr: &str, emails: &[&str], ids: &[&str]) -> KeyRow {
        KeyRow {
            fingerprint: fpr.into(),
            data: vec![1, 2, 3],
            user_ids: emails.iter().map(|e| format!("X <{e}>")).collect(),
            emails: emails.iter().map(|e| e.to_string()).collect(),
            key_ids: ids.iter().map(|e| e.to_string()).collect(),
            algorithm: "Ed25519".into(),
            can_encrypt: true,
            source: "manual".into(),
            ..Default::default()
        }
    }

    #[test]
    fn keys_peers_and_messages() {
        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        db.write(|tx| {
            key_save(tx, &key("AAAA", &["ann@example.com"], &["1111", "2222"]))?;
            key_save(tx, &key("BBBB", &["bob@example.com", "b@x.org"], &["3333"]))?;
            own_set(tx, "Ann@Example.com", "AAAA")?;
            peer_save(
                tx,
                &Peer {
                    email: "bob@example.com".into(),
                    last_seen: 5,
                    autocrypt_at: 5,
                    fingerprint: Some("BBBB".into()),
                    prefer_encrypt: true,
                    ..Default::default()
                },
            )?;
            Ok(())
        })
        .unwrap();
        db.read(|c| {
            assert_eq!(keys_for_email(c, "B@X.org")?[0].fingerprint, "BBBB");
            assert!(keys_for_email(c, "x.org")?.is_empty());
            assert_eq!(keys_by_key_ids(c, &["2222".into()])?[0].fingerprint, "AAAA");
            assert_eq!(keys_list(c, "bob")?.len(), 1);
            assert_eq!(keys_list(c, "")?.len(), 2);
            assert_eq!(keys_list(c, "aa aa")?.len(), 1, "fingerprint with spaces");
            assert_eq!(own_get(c, "ann@EXAMPLE.com")?.as_deref(), Some("AAAA"));
            assert!(peer_get(c, "BOB@example.com")?.unwrap().prefer_encrypt);
            Ok(())
        })
        .unwrap();
        db.write(|tx| key_delete(tx, "BBBB")).unwrap();
        db.read(|c| {
            assert!(key_get(c, "BBBB")?.is_none());
            assert_eq!(peer_get(c, "bob@example.com")?.unwrap().fingerprint, None);
            Ok(())
        })
        .unwrap();
        // Message sources survive compression.
        let raw = b"Content-Type: multipart/encrypted\r\n\r\nxyz".repeat(50);
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO accounts (id, kind, provider, email, name, color, config, created_at)
                 VALUES (1, 'imap', 'generic', 'a@b.c', 'A', '#000', '{}', 0);
                 INSERT INTO messages (id, account_id, thread_id, date) VALUES (7, 1, 1, 0);",
            )?;
            message_crypto_set(tx, 7, "encrypted", &raw)
        })
        .unwrap();
        let got = db.read(|c| message_crypto_get(c, 7)).unwrap().unwrap();
        assert_eq!(got.kind, "encrypted");
        assert_eq!(got.raw.as_deref(), Some(&raw[..]));
        assert!(!got.decrypted);
    }
}
