//! Gmail API settings: send-as aliases and the account profile.

use serde::{Deserialize, Serialize};

use crate::client::{ApiRequest, GoogleClient, Service};
use crate::error::{Error, Result};
use crate::serde_util::{non_empty, null_default};

/// A Gmail send-as alias (an address the user can send mail from).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendAsAlias {
    /// The address in the `From:` header.
    pub email: String,
    /// The name in the `From:` header. For custom addresses Gmail falls back
    /// to the account name when this is empty.
    pub display_name: Option<String>,
    /// `Reply-To:` address, if configured.
    pub reply_to: Option<String>,
    /// HTML signature (as used by the Gmail web UI).
    pub signature_html: Option<String>,
    /// Whether this is the account's own address.
    pub is_primary: bool,
    /// Whether this is the default `From:` address for new messages.
    pub is_default: bool,
    /// Whether Gmail treats this address as an alias of the primary one.
    pub treat_as_alias: bool,
    /// Whether the alias is verified and usable.
    pub verification_status: VerificationStatus,
    /// SMTP relay used for this alias instead of Gmail's servers.
    pub smtp: Option<SmtpRelay>,
}

/// Verification state of a custom send-as address.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VerificationStatus {
    /// The address is verified and can be used.
    Accepted,
    /// Waiting for the owner of the address to verify it.
    Pending,
    /// Not reported (the primary address and plain aliases).
    #[default]
    Unspecified,
}

/// An external SMTP server configured for a send-as alias. Credentials are
/// write-only and never returned by Google.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmtpRelay {
    /// Host name.
    pub host: String,
    /// Port.
    pub port: u16,
    /// `none`, `ssl` or `starttls`.
    pub security_mode: Option<String>,
}

/// The authenticated Gmail account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GmailProfile {
    /// The account's address.
    pub email_address: String,
    /// Total number of messages in the mailbox.
    pub messages_total: u64,
    /// Total number of threads in the mailbox.
    pub threads_total: u64,
    /// Current Gmail history id.
    pub history_id: Option<String>,
}

impl GoogleClient {
    /// Lists the account's send-as aliases, the primary address included.
    pub async fn list_send_as(&self) -> Result<Vec<SendAsAlias>> {
        let request = ApiRequest::get(Service::Gmail, &["users", "me", "settings", "sendAs"]);
        let response: SendAsList = self.send_json(&request, "sendAs.list").await?;
        Ok(response
            .send_as
            .into_iter()
            .filter_map(|alias| {
                Some(SendAsAlias {
                    email: non_empty(alias.send_as_email)?,
                    display_name: non_empty(alias.display_name),
                    reply_to: non_empty(alias.reply_to_address),
                    signature_html: non_empty(alias.signature),
                    is_primary: alias.is_primary.unwrap_or(false),
                    is_default: alias.is_default.unwrap_or(false),
                    treat_as_alias: alias.treat_as_alias.unwrap_or(false),
                    verification_status: match alias.verification_status.as_deref() {
                        Some("accepted") => VerificationStatus::Accepted,
                        Some("pending") => VerificationStatus::Pending,
                        _ => VerificationStatus::Unspecified,
                    },
                    smtp: alias.smtp_msa.and_then(|smtp| {
                        Some(SmtpRelay {
                            host: non_empty(smtp.host)?,
                            port: smtp.port.and_then(|p| u16::try_from(p).ok())?,
                            security_mode: non_empty(smtp.security_mode),
                        })
                    }),
                })
            })
            .collect())
    }

    /// Returns the Gmail profile of the account. Requires a token with the
    /// [`MAIL`](crate::scopes::MAIL) scope (or another Gmail read scope).
    pub async fn gmail_profile(&self) -> Result<GmailProfile> {
        let request = ApiRequest::get(Service::Gmail, &["users", "me", "profile"]);
        let profile: Profile = self.send_json(&request, "users.getProfile").await?;
        Ok(GmailProfile {
            email_address: non_empty(profile.email_address).ok_or_else(|| {
                Error::InvalidResponse("Gmail profile without an e-mail address".to_owned())
            })?,
            messages_total: profile.messages_total.unwrap_or(0),
            threads_total: profile.threads_total.unwrap_or(0),
            history_id: non_empty(profile.history_id),
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendAsList {
    #[serde(default, deserialize_with = "null_default")]
    send_as: Vec<SendAs>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendAs {
    send_as_email: Option<String>,
    display_name: Option<String>,
    reply_to_address: Option<String>,
    signature: Option<String>,
    is_primary: Option<bool>,
    is_default: Option<bool>,
    treat_as_alias: Option<bool>,
    smtp_msa: Option<SmtpMsa>,
    verification_status: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SmtpMsa {
    host: Option<String>,
    port: Option<i64>,
    security_mode: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Profile {
    email_address: Option<String>,
    messages_total: Option<u64>,
    threads_total: Option<u64>,
    history_id: Option<String>,
}
