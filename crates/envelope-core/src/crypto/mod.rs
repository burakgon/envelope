//! End-to-end encryption and signing with OpenPGP, Autocrypt style: keys are
//! created and exchanged in the background so encryption is available
//! whenever every recipient has a key, while experts can import, export,
//! verify and choose keys themselves. See `docs/ENCRYPTION.md`.
//!
//! - Secret keys live in the system keyring (`pgp:secret:<fingerprint>`),
//!   never in the database. Public keys and what is known about them are in
//!   `pgp_keys`; Autocrypt peer state in `pgp_peers`.
//! - Encrypted mail is stored as received (ciphertext). It is decrypted when
//!   opened and kept in memory for the session; only with the "remember
//!   decrypted messages" setting is the decrypted text written to the local
//!   database, where it can be searched.
//! - Drafts of encrypted messages are encrypted to the sender's own key
//!   before they are stored on the server.

mod incoming;
mod keys;
mod outgoing;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

pub use incoming::{OpenedMessage, SignatureView};
pub use keys::{KeyInfo, OwnKey};
pub use outgoing::{ComposeCrypto, RecipientKey};

pub(crate) use incoming::{my_emails, on_headers, on_source};
pub(crate) use outgoing::{apply_plan, plan_outgoing};

use crate::db::misc;
use crate::engine::Engine;
use crate::error::Result;

/// Shown instead of the content of a message that is still encrypted.
pub const ENCRYPTED_SNIPPET: &str = "Encrypted message";

const SETTINGS_KEY: &str = "crypto.settings";

/// Encryption preferences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CryptoSettings {
    /// Encrypt automatically when every recipient has a key (and tell
    /// others so with Autocrypt `prefer-encrypt=mutual`).
    pub encrypt_when_possible: bool,
    /// Sign messages by default.
    pub sign_by_default: bool,
    /// Include the sender's public key in outgoing mail (`Autocrypt`
    /// header); a key is created for an address when it has none.
    pub send_autocrypt: bool,
    /// Replace the subject of encrypted messages with `...` (it travels
    /// inside the encryption instead).
    pub protect_subject: bool,
    /// Keep decrypted copies in the local database so encrypted mail can be
    /// searched and shows a preview.
    pub keep_decrypted: bool,
    /// Look up missing keys in the recipient's domain (Web Key Directory)
    /// while composing.
    pub wkd_lookup: bool,
}

impl Default for CryptoSettings {
    fn default() -> Self {
        CryptoSettings {
            encrypt_when_possible: false,
            sign_by_default: false,
            send_autocrypt: true,
            protect_subject: true,
            keep_decrypted: false,
            wkd_lookup: true,
        }
    }
}

/// A decrypted (or verified) message held for the session.
pub(crate) struct Opened {
    pub view: OpenedMessage,
    /// The decrypted MIME entity, for its attachments and inline images.
    pub entity: Option<Arc<Vec<u8>>>,
}

const OPENED_MAX: usize = 300;

/// Session state: unlocked passphrases and decrypted messages. Nothing here
/// is written to disk.
#[derive(Default)]
pub struct Session {
    passphrases: Mutex<HashMap<String, String>>,
    opened: Mutex<(HashMap<i64, Arc<Opened>>, VecDeque<i64>)>,
    /// When each address was last looked up in a Web Key Directory (ms).
    lookups: Mutex<HashMap<String, i64>>,
}

impl Session {
    pub(crate) fn passphrase(&self, fingerprint: &str) -> Option<String> {
        self.passphrases.lock().get(fingerprint).cloned()
    }

    pub(crate) fn remember_passphrase(&self, fingerprint: &str, passphrase: &str) {
        self.passphrases
            .lock()
            .insert(fingerprint.to_string(), passphrase.to_string());
    }

    pub(crate) fn forget_passphrase(&self, fingerprint: &str) {
        self.passphrases.lock().remove(fingerprint);
    }

    pub(crate) fn opened(&self, message_id: i64) -> Option<Arc<Opened>> {
        self.opened.lock().0.get(&message_id).cloned()
    }

    pub(crate) fn remember_opened(&self, message_id: i64, opened: Arc<Opened>) {
        let mut guard = self.opened.lock();
        let (map, order) = &mut *guard;
        if map.insert(message_id, opened).is_none() {
            order.push_back(message_id);
        }
        while order.len() > OPENED_MAX {
            if let Some(old) = order.pop_front() {
                map.remove(&old);
            }
        }
    }

    /// Drops decrypted messages (keys changed, or the user locked up).
    pub(crate) fn forget_opened(&self) {
        let mut guard = self.opened.lock();
        guard.0.clear();
        guard.1.clear();
    }

    /// Whether `email` may be looked up again (once an hour).
    pub(crate) fn may_look_up(&self, email: &str, now: i64) -> bool {
        let mut map = self.lookups.lock();
        match map.get(email) {
            Some(t) if now - t < 3_600_000 => false,
            _ => {
                map.insert(email.to_string(), now);
                true
            }
        }
    }
}

impl Engine {
    pub fn crypto_settings(&self) -> Result<CryptoSettings> {
        Ok(self
            .db
            .read(|c| misc::kv_get(c, SETTINGS_KEY))?
            .unwrap_or_default())
    }

    pub fn set_crypto_settings(&self, settings: &CryptoSettings) -> Result<()> {
        let before = self.crypto_settings()?;
        self.db
            .write(|tx| misc::kv_set(tx, SETTINGS_KEY, settings))?;
        if before.keep_decrypted && !settings.keep_decrypted {
            self.forget_decrypted_copies()?;
        }
        Ok(())
    }

    /// Forgets unlocked passphrases and decrypted messages of this session.
    pub fn crypto_lock(&self) {
        self.crypto.passphrases.lock().clear();
        self.crypto.forget_opened();
        let _ = std::fs::remove_dir_all(self.paths.cache.join("decrypted"));
    }
}

#[cfg(test)]
mod tests;
