//! Sending: which keys (or S/MIME certificates) to encrypt to, and
//! protecting outgoing messages.

use std::collections::HashSet;

use envelope_pgp::autocrypt;
use envelope_pgp::cert;
use envelope_pgp::ops::Signer;
use envelope_pgp::pgp::composed::{SignedPublicKey, SignedSecretKey};
use envelope_pgp::pgpmime::{self, Protect};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::CryptoSettings;
use super::keys::{KeyInfo, public_of};
use super::smime::{SmimeCompose, SmimePlan};
use crate::compose::Rendered;
use crate::db::crypto::{self as store, KeyRow};
use crate::db::now_ms;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::{Address, DraftInput};

/// A recipient and the key mail to them would be encrypted to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipientKey {
    pub email: String,
    pub key: Option<KeyInfo>,
    /// The recipient asked for encrypted mail (Autocrypt
    /// `prefer-encrypt=mutual`).
    pub prefer_encrypt: bool,
}

/// What the composer can do for a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposeCrypto {
    /// The sending address.
    pub from: String,
    pub own_key: Option<KeyInfo>,
    /// The own key needs its passphrase before it can sign.
    pub own_locked: bool,
    pub recipients: Vec<RecipientKey>,
    /// Recipients without a usable key.
    pub missing: Vec<String>,
    /// Every recipient (and the sender) has a usable key.
    pub can_encrypt: bool,
    pub can_sign: bool,
    /// Every recipient asked for encryption.
    pub mutual: bool,
    pub settings: CryptoSettings,
    /// The same for S/MIME.
    pub smime: SmimeCompose,
    /// The standard a message uses unless the composer chooses:
    /// `openpgp` or `smime`.
    pub standard: String,
}

fn source_rank(source: &str) -> u8 {
    match source {
        "own" | "manual" => 4,
        "wkd" | "keyserver" => 2,
        "autocrypt" => 1,
        _ => 0,
    }
}

/// The key to encrypt to for an address: one the user verified, else one
/// they imported, else the newest Autocrypt key, else one from a key
/// directory, else gossip.
pub(crate) fn select_key(c: &Connection, email: &str, now: i64) -> Result<Option<KeyRow>> {
    let email = email.trim().to_lowercase();
    let peer = store::peer_get(c, &email)?;
    let current = peer.as_ref().and_then(|p| p.fingerprint.clone());
    let mut keys: Vec<KeyRow> = store::keys_for_email(c, &email)?
        .into_iter()
        .filter(|k| k.can_encrypt && !k.revoked && !k.expires_at.is_some_and(|e| e <= now))
        .collect();
    keys.sort_by_key(|k| {
        (
            std::cmp::Reverse(k.verified),
            std::cmp::Reverse(source_rank(&k.source) >= 4),
            std::cmp::Reverse(current.as_deref() == Some(k.fingerprint.as_str())),
            std::cmp::Reverse(source_rank(&k.source)),
            std::cmp::Reverse(k.updated_at),
        )
    });
    // A key must still be able to encrypt now (expiry of subkeys).
    Ok(keys
        .into_iter()
        .find(|k| public_of(k).is_ok_and(|p| !cert::encryption_keys(&p, now / 1000).is_empty())))
}

impl Engine {
    /// Encryption status for the composer: the sender's key and a key for
    /// every recipient, the same for S/MIME certificates, and which
    /// standard to use (`reply_to`: the message being answered).
    pub fn compose_crypto(
        &self,
        account_id: i64,
        identity_id: Option<i64>,
        from: Option<Address>,
        recipients: &[String],
        reply_to: Option<i64>,
    ) -> Result<ComposeCrypto> {
        let draft = DraftInput {
            account_id,
            identity_id,
            from,
            ..Default::default()
        };
        let (sender, _) = crate::sync::draft_identity(self, &draft)?;
        let from = sender.email.trim().to_lowercase();
        let settings = self.crypto_settings()?;
        let now = now_ms();
        let own = self.own_key_for(&from)?;
        let own_info = own.as_ref().map(|k| KeyInfo::from_row(k, now));
        let own_locked = own
            .as_ref()
            .is_some_and(|k| k.protected && self.crypto.passphrase(&k.fingerprint).is_none());
        let mut list = Vec::new();
        let mut missing = Vec::new();
        let mut seen = HashSet::new();
        for r in recipients {
            let email = r.trim().to_lowercase();
            if !email.contains('@') || !seen.insert(email.clone()) {
                continue;
            }
            let (key, peer) = self
                .db
                .read(|c| Ok((select_key(c, &email, now)?, store::peer_get(c, &email)?)))?;
            if key.is_none() {
                missing.push(email.clone());
            }
            list.push(RecipientKey {
                email,
                key: key.map(|k| KeyInfo::from_row(&k, now)),
                prefer_encrypt: peer.is_some_and(|p| p.prefer_encrypt),
            });
        }
        let own_encrypts = own_info.as_ref().is_some_and(|k| k.can_encrypt);
        let can_encrypt = own_encrypts && !list.is_empty() && missing.is_empty();
        let smime = self.smime_compose(&from, recipients)?;
        let standard = self.default_standard(&from, reply_to, &smime, can_encrypt)?;
        Ok(ComposeCrypto {
            can_encrypt,
            can_sign: own_info.as_ref().is_some_and(|k| k.can_sign),
            mutual: !list.is_empty() && list.iter().all(|r| r.prefer_encrypt),
            from,
            own_key: own_info,
            own_locked,
            recipients: list,
            missing,
            settings,
            smime,
            standard: standard.into(),
        })
    }

    /// Whether OpenPGP could encrypt from `from` to every recipient.
    pub(crate) fn pgp_can_encrypt(&self, from: &str, recipients: &[String]) -> Result<bool> {
        let now = now_ms();
        let own = self.own_key_for(&from.trim().to_lowercase())?;
        if !own.is_some_and(|k| KeyInfo::from_row(&k, now).can_encrypt) || recipients.is_empty() {
            return Ok(false);
        }
        self.db.read(|c| {
            for r in recipients {
                if select_key(c, r, now)?.is_none() {
                    return Ok(false);
                }
            }
            Ok(true)
        })
    }
}

/// How an outgoing message is protected.
#[derive(Default)]
pub(crate) struct Plan {
    /// Value of the `Autocrypt` header.
    pub autocrypt: Option<String>,
    to: Vec<SignedPublicKey>,
    hidden: Vec<SignedPublicKey>,
    signer: Option<(SignedSecretKey, Option<String>)>,
    gossip: Vec<String>,
    hide_subject: bool,
    /// Protected with S/MIME instead.
    smime: Option<SmimePlan>,
}

/// Decides how to protect a draft: encryption to every recipient and the
/// sender (Bcc recipients without naming them), a signature, the sender's
/// Autocrypt header — with OpenPGP, or S/MIME when the draft (or the
/// default for its sender) says so. For server copies of drafts
/// (`for_draft`), an encrypted draft is encrypted to the sender alone and
/// nothing is signed.
pub(crate) async fn plan_outgoing(
    engine: &Engine,
    draft: &DraftInput,
    from: &Address,
    for_draft: bool,
) -> Result<Plan> {
    let settings = engine.crypto_settings()?;
    let encrypt = draft.encrypt == Some(true);
    let sign = draft.sign == Some(true) && !for_draft;
    let email = from.email.trim().to_lowercase();
    let smime = (encrypt || sign) && engine.uses_smime(draft, &email)?;
    let mut own = engine.own_key_for(&email)?;
    // An address with an S/MIME certificate gets no OpenPGP key made just
    // for the Autocrypt header: S/MIME would stop being its default.
    let wants_key = if smime {
        false
    } else {
        encrypt || sign || (settings.send_autocrypt && !engine.has_own_certificate(&email)?)
    };
    if own.is_none() && !for_draft && wants_key {
        let name = from.name.clone().unwrap_or_default();
        match engine.ensure_own_key(&email, &name).await {
            Ok(k) => own = Some(k),
            Err(e) if encrypt || sign => return Err(e),
            Err(e) => tracing::warn!("could not create an OpenPGP key for {email}: {e}"),
        }
    }
    let now = now_ms();
    let mut plan = Plan {
        hide_subject: settings.protect_subject,
        ..Default::default()
    };
    if !for_draft
        && settings.send_autocrypt
        && let Some(k) = &own
    {
        let small = cert::minimal(&public_of(k)?, &email, now / 1000);
        plan.autocrypt = Some(autocrypt::format(
            &email,
            settings.encrypt_when_possible,
            &cert::public_bytes(&small)?,
        ));
    }
    if smime {
        plan.smime = Some(engine.plan_smime(draft, &email, for_draft).await?);
        return Ok(plan);
    }
    if encrypt {
        let own = own.as_ref().ok_or_else(|| {
            Error::Invalid(format!("Create a key for {email} to send encrypted mail."))
        })?;
        plan.to.push(public_of(own)?);
        if !for_draft {
            let visible: Vec<&Address> = draft.to.iter().chain(&draft.cc).collect();
            let gossip = visible.len() > 1;
            let mut seen: HashSet<String> = HashSet::from([email.clone()]);
            let mut missing = Vec::new();
            engine.db.read(|c| {
                for (a, hidden) in visible
                    .iter()
                    .map(|a| (*a, false))
                    .chain(draft.bcc.iter().map(|a| (a, true)))
                {
                    let e = a.email.trim().to_lowercase();
                    if !seen.insert(e.clone()) {
                        continue;
                    }
                    let Some(k) = select_key(c, &e, now)? else {
                        missing.push(e);
                        continue;
                    };
                    let public = public_of(&k)?;
                    if hidden {
                        plan.hidden.push(public);
                        continue;
                    }
                    if gossip {
                        let small = cert::minimal(&public, &e, now / 1000);
                        plan.gossip.push(autocrypt::fold(
                            "Autocrypt-Gossip",
                            &autocrypt::format_gossip(&e, &cert::public_bytes(&small)?),
                        ));
                    }
                    plan.to.push(public);
                }
                Ok(())
            })?;
            if !missing.is_empty() {
                return Err(Error::Invalid(format!(
                    "Can't encrypt: there is no key for {}. Turn encryption off or find their key.",
                    missing.join(", ")
                )));
            }
        }
    }
    if sign {
        let own = own
            .as_ref()
            .ok_or_else(|| Error::Invalid(format!("Create a key for {email} to sign mail.")))?;
        let secret = engine.load_secret(&own.fingerprint).await?;
        let passphrase = engine.crypto.passphrase(&own.fingerprint);
        if cert::is_protected(&secret) && passphrase.is_none() {
            return Err(Error::Locked(own.fingerprint.clone()));
        }
        plan.signer = Some((secret, passphrase));
    }
    Ok(plan)
}

/// Encrypts and/or signs a rendered message according to the plan.
pub(crate) fn apply_plan(plan: &Plan, mut rendered: Rendered) -> Result<Rendered> {
    if let Some(smime) = plan.smime.as_ref().filter(|p| !p.is_empty()) {
        rendered.mime = smime.apply(&rendered.mime)?;
        return Ok(rendered);
    }
    if plan.to.is_empty() && plan.hidden.is_empty() && plan.signer.is_none() {
        return Ok(rendered);
    }
    let encrypting = !plan.to.is_empty();
    let protect = Protect {
        encrypt_to: plan.to.iter().collect(),
        hidden: plan.hidden.iter().collect(),
        signer: plan.signer.as_ref().map(|(key, passphrase)| Signer {
            key,
            passphrase: passphrase.as_deref(),
        }),
        protect_headers: encrypting,
        hide_subject: plan.hide_subject,
        inner_headers: plan.gossip.clone(),
    };
    rendered.mime = pgpmime::protect(&rendered.mime, &protect)?;
    Ok(rendered)
}
