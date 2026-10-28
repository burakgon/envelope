//! Sending addresses kept by the provider: Gmail's send-as aliases, with
//! their names, reply-to addresses and signatures, become identities.

use std::collections::HashMap;

use envelope_google::{SendAsAlias, VerificationStatus};

use crate::db::contacts as cdb;
use crate::db::mail::AccountRow;
use crate::db::misc;
use crate::engine::Engine;
use crate::error::Result;
use crate::model::{EngineEvent, Identity};

/// Account state key: the signature last taken from Gmail for each address.
/// A signature still equal to it was not edited in Envelope and may follow
/// changes made in Gmail.
const IMPORTED: &str = "gmailAliases";

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The identity changes for `aliases`: identities to write, and the new
/// record of imported signatures.
fn plan(
    account: &AccountRow,
    existing: &[Identity],
    aliases: &[SendAsAlias],
    imported: &HashMap<String, Option<String>>,
) -> (Vec<Identity>, HashMap<String, Option<String>>) {
    let mut writes = Vec::new();
    let mut record = HashMap::new();
    for alias in aliases {
        if alias.verification_status == VerificationStatus::Pending {
            continue;
        }
        let key = alias.email.to_lowercase();
        let signature = non_empty(alias.signature_html.as_deref());
        match existing
            .iter()
            .find(|i| i.email.eq_ignore_ascii_case(&alias.email))
        {
            Some(identity) => {
                let mut updated = identity.clone();
                let current = non_empty(identity.signature.as_deref());
                let last = imported.get(&key).cloned().flatten();
                if current == last && current != signature {
                    updated.signature = signature.clone();
                }
                if updated.reply_to.is_none() {
                    updated.reply_to = alias.reply_to.clone();
                }
                if updated != *identity {
                    writes.push(updated);
                }
            }
            None => writes.push(Identity {
                id: 0,
                account_id: account.id,
                name: alias
                    .display_name
                    .clone()
                    .unwrap_or_else(|| account.name.clone()),
                email: alias.email.clone(),
                reply_to: alias.reply_to.clone(),
                signature: signature.clone(),
                is_default: false,
            }),
        }
        record.insert(key, signature);
    }
    (writes, record)
}

/// Brings the account's identities in line with its Gmail send-as aliases.
pub(crate) async fn import_gmail_aliases(engine: &Engine, account: &AccountRow) -> Result<()> {
    let client = super::calendar::google_client(engine, account.id)?;
    let aliases = client
        .list_send_as()
        .await
        .map_err(super::calendar::google_err)?;
    let imported: HashMap<String, Option<String>> = account
        .state
        .get(IMPORTED)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let existing: Vec<Identity> = engine
        .identities()?
        .into_iter()
        .filter(|i| i.account_id == account.id)
        .collect();
    let (writes, record) = plan(account, &existing, &aliases, &imported);
    let changed = !writes.is_empty();
    engine.db.write(|tx| {
        for identity in &writes {
            misc::upsert_identity(tx, identity)?;
        }
        cdb::set_account_state_value(tx, account.id, IMPORTED, &serde_json::to_value(&record)?)
    })?;
    if changed {
        engine.emit(EngineEvent::AccountsChanged);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AccountConfig, AccountKind};

    fn alias(email: &str, name: Option<&str>, signature: Option<&str>) -> SendAsAlias {
        SendAsAlias {
            email: email.into(),
            display_name: name.map(Into::into),
            reply_to: None,
            signature_html: signature.map(Into::into),
            is_primary: false,
            is_default: false,
            treat_as_alias: true,
            verification_status: VerificationStatus::Accepted,
            smtp: None,
        }
    }

    fn identity(id: i64, email: &str, signature: Option<&str>) -> Identity {
        Identity {
            id,
            account_id: 1,
            name: "Ada".into(),
            email: email.into(),
            reply_to: None,
            signature: signature.map(Into::into),
            is_default: id == 1,
        }
    }

    #[test]
    fn imports_aliases_without_overwriting_local_edits() {
        let account = AccountRow {
            id: 1,
            kind: AccountKind::Gmail,
            provider: "gmail".into(),
            email: "ada@gmail.com".into(),
            name: "Ada Lovelace".into(),
            label: None,
            color: String::new(),
            config: AccountConfig::default(),
            state: serde_json::Value::Null,
            sort_order: 0,
            enabled: true,
        };
        let mut pending = alias("new@example.com", None, None);
        pending.verification_status = VerificationStatus::Pending;
        let aliases = [
            alias("ada@gmail.com", Some("Ada"), Some("<b>Ada</b>")),
            alias("Work@Example.com", Some("Ada L."), Some("<i>Work</i>")),
            alias("edited@example.com", None, Some("<p>Gmail v2</p>")),
            pending,
        ];
        let existing = [
            identity(1, "ada@gmail.com", None),
            identity(2, "edited@example.com", Some("<p>My own</p>")),
        ];
        let imported = HashMap::from([(
            "edited@example.com".to_string(),
            Some("<p>Gmail v1</p>".to_string()),
        )]);
        let (writes, record) = plan(&account, &existing, &aliases, &imported);

        // The primary address takes Gmail's signature (it had none).
        assert_eq!(writes[0].id, 1);
        assert_eq!(writes[0].signature.as_deref(), Some("<b>Ada</b>"));
        // A new alias becomes an identity with its own name.
        assert_eq!(writes[1].id, 0);
        assert_eq!(writes[1].name, "Ada L.");
        assert_eq!(writes[1].email, "Work@Example.com");
        // A signature edited in Envelope stays; pending aliases are skipped.
        assert_eq!(writes.len(), 2);
        assert_eq!(
            record.get("edited@example.com"),
            Some(&Some("<p>Gmail v2</p>".to_string()))
        );
        assert!(!record.contains_key("new@example.com"));

        // Once imported, a later Gmail change follows through.
        let synced = [
            identity(1, "ada@gmail.com", Some("<b>Ada</b>")),
            identity(3, "work@example.com", Some("<i>Work</i>")),
        ];
        let changed = [
            alias("ada@gmail.com", Some("Ada"), Some("<b>Ada Lovelace</b>")),
            alias("work@example.com", Some("Ada L."), Some("<i>Work</i>")),
        ];
        let (writes, _) = plan(&account, &synced, &changed, &record);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].signature.as_deref(), Some("<b>Ada Lovelace</b>"));
    }
}
