//! The signed-in user's profile and mailbox settings.

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::client::{GraphClient, GraphRequest};
use crate::error::GraphError;
use crate::serde_util::{graph_enum, null_default};
use crate::types::DateTimeTimeZone;

/// Basic profile of the signed-in user (`GET /me`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfile {
    /// Directory object id of the user.
    pub id: String,
    /// Display name.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Primary SMTP address. Often empty for personal Microsoft accounts.
    #[serde(default)]
    pub mail: Option<String>,
    /// User principal name (the sign-in name).
    #[serde(default)]
    pub user_principal_name: Option<String>,
}

impl UserProfile {
    /// The best e-mail address for the user: `mail`, falling back to the user
    /// principal name when that looks like an address.
    pub fn email(&self) -> Option<&str> {
        self.mail
            .as_deref()
            .filter(|m| !m.trim().is_empty())
            .or_else(|| {
                self.user_principal_name
                    .as_deref()
                    .filter(|upn| upn.contains('@'))
            })
    }
}

graph_enum! {
    /// Whether automatic replies (out-of-office) are on.
    pub enum AutomaticRepliesStatus {
        /// Automatic replies are off.
        Disabled = "disabled",
        /// Automatic replies are on until turned off.
        AlwaysEnabled = "alwaysEnabled",
        /// Automatic replies are on between the scheduled start and end.
        Scheduled = "scheduled",
    }
    unknown = Disabled;
    default = Disabled;
}

graph_enum! {
    /// Which external senders receive the external automatic reply.
    pub enum ExternalAudienceScope {
        /// No external senders.
        None = "none",
        /// Only senders in the user's contacts.
        ContactsOnly = "contactsOnly",
        /// All external senders.
        All = "all",
    }
    unknown = None;
    default = None;
}

/// Automatic replies ("out of office") configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomaticRepliesSetting {
    /// Current state.
    #[serde(default, deserialize_with = "null_default")]
    pub status: AutomaticRepliesStatus,
    /// Audience of the external reply.
    #[serde(default, deserialize_with = "null_default")]
    pub external_audience: ExternalAudienceScope,
    /// Start of the scheduled period (required with
    /// [`AutomaticRepliesStatus::Scheduled`]; must lie in the future when set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_start_date_time: Option<DateTimeTimeZone>,
    /// End of the scheduled period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_end_date_time: Option<DateTimeTimeZone>,
    /// Reply sent to senders inside the organization (HTML).
    #[serde(default, deserialize_with = "null_default")]
    pub internal_reply_message: String,
    /// Reply sent to external senders (HTML).
    #[serde(default, deserialize_with = "null_default")]
    pub external_reply_message: String,
}

/// A locale such as `en-US`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocaleInfo {
    /// Locale code (`en-US`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    /// Display name (`English (United States)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// Mailbox settings of the signed-in user (`GET /me/mailboxSettings`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailboxSettings {
    /// Default time zone of the mailbox, usually a Windows name such as
    /// `Pacific Standard Time` (IANA names are possible).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    /// Out-of-office configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automatic_replies_setting: Option<AutomaticRepliesSetting>,
    /// Preferred language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<LocaleInfo>,
    /// Preferred date format, e.g. `MM/dd/yyyy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_format: Option<String>,
    /// Preferred time format, e.g. `hh:mm tt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_format: Option<String>,
}

#[derive(Deserialize)]
struct StringValue {
    #[serde(default)]
    value: String,
}

impl GraphClient {
    /// The signed-in user's id, display name, mail and user principal name.
    pub async fn me(&self) -> Result<UserProfile, GraphError> {
        let request =
            GraphRequest::get(["me"]).query("$select", "id,displayName,mail,userPrincipalName");
        self.send_json(&request).await
    }

    /// All mailbox settings the crate understands.
    pub async fn mailbox_settings(&self) -> Result<MailboxSettings, GraphError> {
        self.send_json(&GraphRequest::get(["me", "mailboxSettings"]))
            .await
    }

    /// The mailbox time zone (Windows or IANA name).
    pub async fn mailbox_time_zone(&self) -> Result<String, GraphError> {
        let value: StringValue = self
            .send_json(&GraphRequest::get(["me", "mailboxSettings", "timeZone"]))
            .await?;
        Ok(value.value)
    }

    /// Changes the mailbox time zone (Windows or IANA name).
    pub async fn set_mailbox_time_zone(&self, time_zone: &str) -> Result<(), GraphError> {
        let request = GraphRequest::patch(["me", "mailboxSettings"])
            .json(&json!({ "timeZone": time_zone }))?;
        self.send_empty(&request).await
    }

    /// The current automatic replies (out-of-office) configuration.
    pub async fn automatic_replies(&self) -> Result<AutomaticRepliesSetting, GraphError> {
        let request = GraphRequest::get(["me", "mailboxSettings", "automaticRepliesSetting"]);
        self.send_json(&request).await
    }

    /// Replaces the automatic replies (out-of-office) configuration.
    pub async fn set_automatic_replies(
        &self,
        setting: &AutomaticRepliesSetting,
    ) -> Result<(), GraphError> {
        if setting.status == AutomaticRepliesStatus::Scheduled
            && (setting.scheduled_start_date_time.is_none()
                || setting.scheduled_end_date_time.is_none())
        {
            return Err(GraphError::InvalidInput(
                "scheduled automatic replies need a start and an end".into(),
            ));
        }
        let request = GraphRequest::patch(["me", "mailboxSettings"])
            .json(&json!({ "automaticRepliesSetting": setting }))?;
        self.send_empty(&request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_falls_back_to_upn() {
        let mut profile = UserProfile {
            id: "1".into(),
            display_name: None,
            mail: None,
            user_principal_name: Some("someone@outlook.com".into()),
        };
        assert_eq!(profile.email(), Some("someone@outlook.com"));
        profile.mail = Some("first.last@contoso.com".into());
        assert_eq!(profile.email(), Some("first.last@contoso.com"));
        profile.mail = Some(String::new());
        profile.user_principal_name = Some("not-an-address".into());
        assert_eq!(profile.email(), None);
    }

    #[test]
    fn parses_capitalized_enum_values() {
        let setting: AutomaticRepliesSetting = serde_json::from_str(
            r#"{"status":"Scheduled","externalAudience":"All","internalReplyMessage":null}"#,
        )
        .unwrap_or_default();
        assert_eq!(setting.status, AutomaticRepliesStatus::Scheduled);
        assert_eq!(setting.external_audience, ExternalAudienceScope::All);
        assert_eq!(setting.internal_reply_message, "");
    }
}
