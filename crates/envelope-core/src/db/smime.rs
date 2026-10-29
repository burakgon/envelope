//! S/MIME certificates (`smime_certs`).

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use super::crypto::{spaced, unspaced};
use crate::error::Result;

/// A certificate as stored.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CertRow {
    /// SHA-256 of the DER, upper-case hex.
    pub fingerprint: String,
    /// DER.
    pub data: Vec<u8>,
    /// Lower case.
    pub emails: Vec<String>,
    /// An authority's certificate.
    pub ca: bool,
    /// The private key is in the keyring.
    pub secret: bool,
    /// `own`, `message`, `manual` or `chain`.
    pub source: String,
    pub added_at: i64,
    pub updated_at: i64,
}

const COLUMNS: &str = "fingerprint, data, emails, ca, secret, source, added_at, updated_at";

fn from_row(r: &Row<'_>) -> rusqlite::Result<CertRow> {
    Ok(CertRow {
        fingerprint: r.get(0)?,
        data: r.get(1)?,
        emails: unspaced(&r.get::<_, String>(2)?),
        ca: r.get::<_, i64>(3)? != 0,
        secret: r.get::<_, i64>(4)? != 0,
        source: r.get(5)?,
        added_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

/// Inserts or replaces a certificate.
pub fn cert_save(tx: &Transaction<'_>, c: &CertRow) -> Result<()> {
    tx.execute(
        &format!(
            "INSERT INTO smime_certs ({COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (fingerprint) DO UPDATE SET data = excluded.data, emails = excluded.emails,
               ca = excluded.ca, secret = excluded.secret, source = excluded.source,
               updated_at = excluded.updated_at"
        ),
        params![
            c.fingerprint,
            c.data,
            spaced(&c.emails),
            c.ca as i64,
            c.secret as i64,
            c.source,
            c.added_at,
            c.updated_at,
        ],
    )?;
    Ok(())
}

pub fn cert_get(c: &Connection, fingerprint: &str) -> Result<Option<CertRow>> {
    Ok(c.query_row(
        &format!("SELECT {COLUMNS} FROM smime_certs WHERE fingerprint = ?"),
        [fingerprint],
        from_row,
    )
    .optional()?)
}

pub fn cert_delete(tx: &Transaction<'_>, fingerprint: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM smime_certs WHERE fingerprint = ?",
        [fingerprint],
    )?;
    Ok(())
}

fn query(c: &Connection, filter: &str, args: impl rusqlite::Params) -> Result<Vec<CertRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM smime_certs WHERE {filter} ORDER BY secret DESC, emails, fingerprint"
    ))?;
    let rows = st.query_map(args, from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// People's certificates (the user's own first), without authorities.
pub fn certs_list(c: &Connection) -> Result<Vec<CertRow>> {
    query(c, "ca = 0", [])
}

/// Certificates for an address.
pub fn certs_for_email(c: &Connection, email: &str) -> Result<Vec<CertRow>> {
    query(
        c,
        "ca = 0 AND instr(emails, ' ' || ? || ' ') > 0",
        [email.trim().to_lowercase()],
    )
}

/// The user's own certificates (private key in the keyring).
pub fn certs_secret(c: &Connection) -> Result<Vec<CertRow>> {
    query(c, "secret = 1", [])
}

/// Authorities' certificates.
pub fn certs_ca(c: &Connection) -> Result<Vec<CertRow>> {
    query(c, "ca = 1", [])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn certificates_by_address_and_kind() {
        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        let row = |fp: &str, emails: &[&str], ca: bool, secret: bool| CertRow {
            fingerprint: fp.into(),
            data: vec![1, 2, 3],
            emails: emails.iter().map(|e| e.to_string()).collect(),
            ca,
            secret,
            source: if secret { "own" } else { "message" }.into(),
            added_at: 1,
            updated_at: 1,
        };
        db.write(|tx| {
            cert_save(tx, &row("AA", &["ann@example.com"], false, true))?;
            cert_save(tx, &row("BB", &["bob@example.org"], false, false))?;
            cert_save(tx, &row("CA", &[], true, false))?;
            Ok(())
        })
        .unwrap();
        db.read(|c| {
            assert_eq!(certs_for_email(c, "Bob@Example.org")?[0].fingerprint, "BB");
            assert!(certs_for_email(c, "example.org")?.is_empty());
            assert_eq!(certs_list(c)?.len(), 2);
            assert_eq!(certs_list(c)?[0].fingerprint, "AA", "own first");
            assert_eq!(certs_secret(c)?.len(), 1);
            assert_eq!(certs_ca(c)?[0].fingerprint, "CA");
            Ok(())
        })
        .unwrap();
        db.write(|tx| cert_delete(tx, "BB")).unwrap();
        assert!(db.read(|c| cert_get(c, "BB")).unwrap().is_none());
    }
}
