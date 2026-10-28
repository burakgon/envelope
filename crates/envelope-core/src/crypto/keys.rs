//! Keys: the user's own (generated or imported, secret part in the
//! keyring) and other people's (Autocrypt, Web Key Directory,
//! keys.openpgp.org, imported by hand).

use std::collections::HashSet;
use std::time::Duration;

use envelope_pgp::cert::{self, Cert};
use envelope_pgp::pgp::composed::{SignedPublicKey, SignedSecretKey};
use envelope_pgp::wkd;
use serde::{Deserialize, Serialize};

use crate::db::crypto::{self as store, KeyRow};
use crate::db::{misc, now_ms};
use crate::engine::Engine;
use crate::error::{Error, Result};

/// Name of a secret key in the keyring.
pub(crate) fn secret_name(fingerprint: &str) -> String {
    format!("pgp:secret:{fingerprint}")
}

/// A certificate as the interface shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyInfo {
    /// Upper-case hex fingerprint.
    pub fingerprint: String,
    pub user_ids: Vec<String>,
    pub emails: Vec<String>,
    pub algorithm: String,
    /// Unix ms.
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub revoked: bool,
    pub expired: bool,
    pub can_encrypt: bool,
    pub can_sign: bool,
    /// The secret key is in the keyring (one of the user's own keys).
    pub secret: bool,
    /// The secret key is protected by a passphrase.
    pub protected: bool,
    /// Where the key came from: `own`, `autocrypt`, `gossip`, `wkd`,
    /// `keyserver` or `manual`.
    pub source: String,
    /// The user compared the fingerprint with its owner.
    pub verified: bool,
    pub added_at: i64,
    pub updated_at: i64,
}

impl KeyInfo {
    pub(crate) fn from_row(r: &KeyRow, now: i64) -> KeyInfo {
        let expired = r.expires_at.is_some_and(|e| e <= now);
        KeyInfo {
            fingerprint: r.fingerprint.clone(),
            user_ids: r.user_ids.clone(),
            emails: r.emails.clone(),
            algorithm: r.algorithm.clone(),
            created_at: r.created_at,
            expires_at: r.expires_at,
            revoked: r.revoked,
            expired,
            can_encrypt: r.can_encrypt && !expired && !r.revoked,
            can_sign: r.can_sign && !expired && !r.revoked,
            secret: r.secret,
            protected: r.protected,
            source: r.source.clone(),
            verified: r.verified,
            added_at: r.added_at,
            updated_at: r.updated_at,
        }
    }
}

/// An address the user sends from, and its key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnKey {
    pub email: String,
    pub name: String,
    pub identity_ids: Vec<i64>,
    pub key: Option<KeyInfo>,
    /// The key needs no passphrase, or it was entered in this session.
    pub unlocked: bool,
}

fn source_rank(source: &str) -> u8 {
    match source {
        "own" => 6,
        "manual" => 5,
        "wkd" => 4,
        "keyserver" => 3,
        "autocrypt" => 2,
        "gossip" => 1,
        _ => 0,
    }
}

/// Stores a certificate, merging it with what is already known about it
/// (new subkeys, user ids, revocations) and keeping the user's verdicts.
pub(crate) fn store_cert(
    tx: &rusqlite::Transaction<'_>,
    cert: &Cert,
    source: &str,
    extra_emails: &[String],
) -> Result<KeyRow> {
    let fingerprint = cert.fingerprint();
    let existing = store::key_get(tx, &fingerprint)?;
    let public = match existing
        .as_ref()
        .and_then(|old| cert::parse_public(&old.data).ok())
    {
        Some(old) => cert::merge(&old, &cert.public),
        None => cert.public.clone(),
    };
    let merged = Cert {
        public,
        secret: cert.secret.clone(),
    };
    let info = merged.info();
    let mut emails = info.emails.clone();
    for e in extra_emails
        .iter()
        .chain(existing.iter().flat_map(|o| o.emails.iter()))
    {
        let e = e.trim().to_lowercase();
        if e.contains('@') && !emails.contains(&e) {
            emails.push(e);
        }
    }
    let now = now_ms();
    let source = match &existing {
        Some(old) if source_rank(&old.source) >= source_rank(source) => old.source.clone(),
        _ => source.to_string(),
    };
    let row = KeyRow {
        fingerprint,
        data: cert::public_bytes(&merged.public)?,
        user_ids: info.user_ids,
        emails,
        key_ids: info.key_ids,
        algorithm: info.algorithm,
        created_at: info.created_at * 1000,
        expires_at: info.expires_at.map(|e| e * 1000),
        revoked: info.revoked,
        can_encrypt: info.can_encrypt,
        can_sign: info.can_sign,
        secret: info.secret || existing.as_ref().is_some_and(|o| o.secret),
        protected: if info.secret {
            info.protected
        } else {
            existing.as_ref().is_some_and(|o| o.protected)
        },
        source,
        verified: existing.as_ref().is_some_and(|o| o.verified),
        added_at: existing.as_ref().map(|o| o.added_at).unwrap_or(now),
        updated_at: now,
    };
    store::key_save(tx, &row)?;
    Ok(row)
}

/// The public certificate of a stored key.
pub(crate) fn public_of(row: &KeyRow) -> Result<SignedPublicKey> {
    Ok(cert::parse_public(&row.data)?)
}

impl Engine {
    /// Every address the user sends from with its key.
    pub fn own_keys(&self) -> Result<Vec<OwnKey>> {
        let identities = self.db.read(misc::list_identities)?;
        let now = now_ms();
        let mut out: Vec<OwnKey> = Vec::new();
        for i in identities {
            let email = i.email.trim().to_lowercase();
            if email.is_empty() {
                continue;
            }
            if let Some(o) = out.iter_mut().find(|o| o.email == email) {
                o.identity_ids.push(i.id);
                continue;
            }
            let key = self.own_key_for(&email)?;
            let unlocked = key
                .as_ref()
                .is_some_and(|k| !k.protected || self.crypto.passphrase(&k.fingerprint).is_some());
            out.push(OwnKey {
                email,
                name: i.name,
                identity_ids: vec![i.id],
                key: key.map(|k| KeyInfo::from_row(&k, now)),
                unlocked,
            });
        }
        Ok(out)
    }

    /// Creates a key (Ed25519 with a Curve25519 encryption subkey) for an
    /// address and makes it the address's key.
    pub async fn generate_key(
        &self,
        email: &str,
        name: &str,
        passphrase: Option<&str>,
    ) -> Result<KeyInfo> {
        let email = email.trim().to_lowercase();
        let passphrase = passphrase.filter(|p| !p.is_empty());
        let (n, e, p) = (
            name.to_string(),
            email.clone(),
            passphrase.map(str::to_string),
        );
        let key = tokio::task::spawn_blocking(move || cert::generate(&n, &e, p.as_deref()))
            .await
            .map_err(|e| Error::Other(format!("key generation failed: {e}")))??;
        let row = self
            .add_own_secret(&key, std::slice::from_ref(&email), Some(&email))
            .await?;
        if let Some(p) = passphrase {
            self.crypto.remember_passphrase(&row.fingerprint, p);
        }
        self.crypto.forget_opened();
        Ok(KeyInfo::from_row(&row, now_ms()))
    }

    /// Stores a secret key in the keyring and its certificate in the
    /// database; it becomes the key of `use_for`, or of the user's addresses
    /// it carries that have no key yet.
    async fn add_own_secret(
        &self,
        key: &SignedSecretKey,
        extra_emails: &[String],
        use_for: Option<&str>,
    ) -> Result<KeyRow> {
        let cert = Cert::from_secret(key.clone());
        let fingerprint = cert.fingerprint();
        self.secrets
            .set(&secret_name(&fingerprint), &cert::secret_armored(key)?)
            .await?;
        let mine: HashSet<String> = self
            .db
            .read(misc::list_identities)?
            .into_iter()
            .map(|i| i.email.trim().to_lowercase())
            .collect();
        self.db.write(|tx| {
            let row = store_cert(tx, &cert, "own", extra_emails)?;
            match use_for {
                Some(email) => store::own_set(tx, email, &row.fingerprint)?,
                None => {
                    for e in &row.emails {
                        if mine.contains(e) && store::own_get(tx, e)?.is_none() {
                            store::own_set(tx, e, &row.fingerprint)?;
                        }
                    }
                }
            }
            Ok(row)
        })
    }

    /// Imports keys (armored or binary, public or secret, one or many).
    /// Secret keys become the user's own; `passphrase` is checked against
    /// protected ones and remembered for the session. With `own_email`, an
    /// imported secret key becomes that address's key.
    pub async fn import_keys(
        &self,
        data: &[u8],
        passphrase: Option<&str>,
        own_email: Option<&str>,
    ) -> Result<Vec<KeyInfo>> {
        let certs = cert::parse(data)?;
        let passphrase = passphrase.filter(|p| !p.is_empty());
        let own_email = own_email
            .map(|e| e.trim().to_lowercase())
            .filter(|e| !e.is_empty());
        let now = now_ms();
        let mut out = Vec::new();
        for c in certs {
            let row = match &c.secret {
                Some(secret) => {
                    let protected = cert::is_protected(secret);
                    if protected && let Some(p) = passphrase {
                        cert::check_passphrase(secret, p)?;
                    }
                    let extra: Vec<String> = own_email.iter().cloned().collect();
                    let row = self
                        .add_own_secret(secret, &extra, own_email.as_deref())
                        .await?;
                    if protected && let Some(p) = passphrase {
                        self.crypto.remember_passphrase(&row.fingerprint, p);
                    }
                    row
                }
                None => self.db.write(|tx| store_cert(tx, &c, "manual", &[]))?,
            };
            out.push(KeyInfo::from_row(&row, now));
        }
        self.crypto.forget_opened();
        Ok(out)
    }

    /// The public key, ASCII armored.
    pub fn export_public_key(&self, fingerprint: &str) -> Result<String> {
        let row = self.key_row(fingerprint)?;
        Ok(cert::public_armored(&public_of(&row)?)?)
    }

    /// The secret key, ASCII armored. A key without a passphrase can be
    /// protected with `passphrase` for the exported copy.
    pub async fn export_secret_key(
        &self,
        fingerprint: &str,
        passphrase: Option<&str>,
    ) -> Result<String> {
        let key = self.load_secret(fingerprint).await?;
        let key = match passphrase.filter(|p| !p.is_empty()) {
            Some(p) if !cert::is_protected(&key) => cert::set_passphrase(&key, p)?,
            _ => key,
        };
        Ok(cert::secret_armored(&key)?)
    }

    /// Deletes a key (and, for the user's own keys, the secret key in the
    /// keyring).
    pub async fn delete_key(&self, fingerprint: &str) -> Result<()> {
        let row = self.key_row(fingerprint)?;
        if row.secret {
            self.secrets.delete(&secret_name(&row.fingerprint)).await?;
        }
        self.db
            .write(|tx| store::key_delete(tx, &row.fingerprint))?;
        self.crypto.forget_passphrase(&row.fingerprint);
        self.crypto.forget_opened();
        Ok(())
    }

    /// Keys matching an address, a name or (part of) a fingerprint.
    pub fn keys(&self, query: &str) -> Result<Vec<KeyInfo>> {
        let now = now_ms();
        Ok(self
            .db
            .read(|c| store::keys_list(c, query))?
            .iter()
            .map(|r| KeyInfo::from_row(r, now))
            .collect())
    }

    /// Records that the user compared a key's fingerprint with its owner.
    pub fn set_key_verified(&self, fingerprint: &str, verified: bool) -> Result<()> {
        let found = self
            .db
            .write(|tx| store::key_set_verified(tx, fingerprint, verified))?;
        if !found {
            return Err(Error::NotFound(format!("key {fingerprint}")));
        }
        self.crypto.forget_opened();
        Ok(())
    }

    /// Checks a passphrase and remembers it for the session.
    pub async fn unlock_key(&self, fingerprint: &str, passphrase: &str) -> Result<()> {
        let key = self.load_secret(fingerprint).await?;
        if cert::is_protected(&key) {
            cert::check_passphrase(&key, passphrase)?;
        }
        self.crypto.remember_passphrase(fingerprint, passphrase);
        self.crypto.forget_opened();
        Ok(())
    }

    /// Makes one of the user's keys the key of an address.
    pub fn set_own_key(&self, email: &str, fingerprint: &str) -> Result<()> {
        let row = self.key_row(fingerprint)?;
        if !row.secret {
            return Err(Error::Invalid(
                "Only keys whose secret part you have can be used to send".into(),
            ));
        }
        self.db
            .write(|tx| store::own_set(tx, email, &row.fingerprint))
    }

    pub(crate) fn key_row(&self, fingerprint: &str) -> Result<KeyRow> {
        let fingerprint = fingerprint.trim().replace(' ', "").to_uppercase();
        self.db
            .read(|c| store::key_get(c, &fingerprint))?
            .ok_or_else(|| Error::NotFound(format!("key {fingerprint}")))
    }

    /// A secret key from the keyring.
    pub(crate) async fn load_secret(&self, fingerprint: &str) -> Result<SignedSecretKey> {
        let armored = self
            .secrets
            .get(&secret_name(fingerprint))
            .await?
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "The secret key {fingerprint} is missing from the keyring."
                ))
            })?;
        Ok(cert::parse_secret(armored.as_bytes())?)
    }

    /// The user's key for an address.
    pub(crate) fn own_key_for(&self, email: &str) -> Result<Option<KeyRow>> {
        self.db.read(|c| {
            let Some(fingerprint) = store::own_get(c, email)? else {
                return Ok(None);
            };
            Ok(store::key_get(c, &fingerprint)?.filter(|k| k.secret))
        })
    }

    /// The user's key for an address, created when there is none.
    pub(crate) async fn ensure_own_key(&self, email: &str, name: &str) -> Result<KeyRow> {
        if let Some(row) = self.own_key_for(email)? {
            return Ok(row);
        }
        let info = self.generate_key(email, name, None).await?;
        self.key_row(&info.fingerprint)
    }

    /// Looks up keys for addresses in their Web Key Directory and, when
    /// `keyserver` is set (an explicit request), on keys.openpgp.org.
    /// Automatic lookups (`explicit` false) ask each address at most once an
    /// hour and only when the setting allows it.
    pub async fn lookup_keys(
        &self,
        emails: &[String],
        keyserver: bool,
        explicit: bool,
    ) -> Result<Vec<KeyInfo>> {
        let settings = self.crypto_settings()?;
        if !explicit && !settings.wkd_lookup {
            return Ok(Vec::new());
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .build()?;
        let now = now_ms();
        let mut seen = HashSet::new();
        let mut found = Vec::new();
        for email in emails {
            let email = email.trim().to_lowercase();
            if !email.contains('@') || !seen.insert(email.clone()) {
                continue;
            }
            if !explicit && !self.crypto.may_look_up(&email, now) {
                continue;
            }
            let mut sources: Vec<(Vec<u8>, &str)> = Vec::new();
            if let Some(urls) = wkd::urls(&email) {
                match fetch_key(&client, &urls.advanced).await {
                    Some(data) => sources.push((data, "wkd")),
                    None => {
                        if let Some(data) = fetch_key(&client, &urls.direct).await {
                            sources.push((data, "wkd"));
                        }
                    }
                }
            }
            if keyserver
                && explicit
                && let Some(data) = fetch_key(&client, &wkd::keys_openpgp_org(&email)).await
            {
                sources.push((data, "keyserver"));
            }
            for (data, source) in sources {
                for key in self.take_found_keys(&email, &data, source)? {
                    if !found
                        .iter()
                        .any(|k: &KeyInfo| k.fingerprint == key.fingerprint)
                    {
                        found.push(key);
                    }
                }
            }
        }
        if !found.is_empty() {
            self.crypto.forget_opened();
        }
        Ok(found)
    }

    /// Stores the keys in downloaded data that belong to `email` and can
    /// encrypt; anything else a directory sends along is ignored.
    pub(crate) fn take_found_keys(
        &self,
        email: &str,
        data: &[u8],
        source: &str,
    ) -> Result<Vec<KeyInfo>> {
        let now = now_ms();
        let mut out = Vec::new();
        for c in cert::parse(data).unwrap_or_default() {
            let info = c.info();
            if c.secret.is_some() || !info.emails.iter().any(|e| e == email) || !info.can_encrypt {
                continue;
            }
            let c = Cert::from_public(c.public);
            let row = self.db.write(|tx| store_cert(tx, &c, source, &[]))?;
            out.push(KeyInfo::from_row(&row, now));
        }
        Ok(out)
    }
}

/// Downloads a key from a URL (at most 1 MB); `None` when there is none.
pub(crate) async fn fetch_key(client: &reqwest::Client, url: &str) -> Option<Vec<u8>> {
    let resp = client
        .get(url)
        .header("Accept", "application/octet-stream, application/pgp-keys")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    if resp.content_length().is_some_and(|l| l > 1_048_576) {
        return None;
    }
    let data = resp.bytes().await.ok()?;
    (!data.is_empty() && data.len() <= 1_048_576).then(|| data.to_vec())
}
