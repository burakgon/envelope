//! Automatic replies ("out of office", Gmail's "vacation responder") for
//! every kind of account: Microsoft Graph mailbox settings, Exchange Web
//! Services, the Gmail settings API and ManageSieve on IMAP servers.

use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::calendar::local_tz;
use crate::db::mail::AccountRow;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::mime::html_to_text;
use crate::model::{AccountKind, AuthMethod, Security};
use crate::sieve;

/// Who gets the automatic reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Audience {
    #[default]
    Everyone,
    /// People in the user's contacts (and, for Exchange and Microsoft 365,
    /// everyone inside the organization).
    Contacts,
    /// Only people inside the user's organization or Workspace domain.
    Organization,
}

/// An account's automatic reply.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoReply {
    pub enabled: bool,
    /// First and last day (inclusive) of the reply; both or neither.
    pub start: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
    /// Subject of the reply, where the account supports one.
    pub subject: String,
    /// Plain text.
    pub message: String,
    pub audience: Audience,
}

/// The reply with what the account can do.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoReplyInfo {
    pub reply: AutoReply,
    /// The audiences the account offers.
    pub audiences: Vec<Audience>,
    /// The reply can have its own subject.
    pub subject: bool,
}

/// IMAP providers known not to offer ManageSieve.
const NO_SIEVE: &[&str] = &[
    "imap.gmail.com",
    "imap.mail.me.com",
    "imap.mail.yahoo.com",
    "imap.aol.com",
    "outlook.office365.com",
    "imap-mail.outlook.com",
    "imap.fastmail.com",
    "imap.zoho.com",
    "imap.zoho.eu",
    "imap.gmx.com",
    "imap.gmx.net",
    "imap.web.de",
    "imap.yandex.com",
];

fn unsupported() -> Error {
    Error::Invalid("This account's mail server does not offer automatic replies.".into())
}

/// Local midnight at the start of `day` (or the first moment of the day
/// where a clock change skips midnight).
fn day_start(day: NaiveDate) -> DateTime<Utc> {
    let tz = local_tz();
    let midnight = day.and_hms_opt(0, 0, 0).unwrap_or_default();
    (0..=12)
        .find_map(|half_hours| {
            tz.from_local_datetime(&(midnight + Duration::minutes(30 * half_hours)))
                .earliest()
        })
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|| midnight.and_utc())
}

fn local_day(at: DateTime<Utc>) -> NaiveDate {
    at.with_timezone(&local_tz()).date_naive()
}

/// The period of a reply as instants: from the first day's start to the end
/// of the last day (exclusive). Either side may be open (Gmail and Sieve
/// allow that; Exchange and Microsoft 365 want both).
fn period(reply: &AutoReply) -> Result<(Option<DateTime<Utc>>, Option<DateTime<Utc>>)> {
    if let (Some(start), Some(end)) = (reply.start, reply.end)
        && start > end
    {
        return Err(Error::Invalid(
            "The last day of the automatic reply comes before the first.".into(),
        ));
    }
    Ok((
        reply.start.map(day_start),
        reply.end.map(|e| day_start(e + Duration::days(1))),
    ))
}

/// Both ends of a period, for services that schedule only closed ones.
fn closed(
    period: (Option<DateTime<Utc>>, Option<DateTime<Utc>>),
) -> Result<Option<(DateTime<Utc>, DateTime<Utc>)>> {
    match period {
        (None, None) => Ok(None),
        (Some(start), Some(end)) => Ok(Some((start, end))),
        _ => Err(Error::Invalid(
            "Choose both the first and the last day of the automatic reply.".into(),
        )),
    }
}

/// The days of a period given as instants (the end being exclusive).
fn days(
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
) -> (Option<NaiveDate>, Option<NaiveDate>) {
    (
        start.map(local_day),
        end.map(|e| local_day(e - Duration::milliseconds(1))),
    )
}

fn text_to_html(text: &str) -> String {
    html_escape::encode_text(text).replace('\n', "<br>")
}

/// The HTML to store: the server's own when the text was not changed (so
/// formatting made elsewhere survives), else the text as HTML.
fn html_for(text: &str, current_html: &str) -> String {
    if !current_html.trim().is_empty() && html_to_text(current_html).trim() == text.trim() {
        current_html.to_string()
    } else {
        text_to_html(text)
    }
}

fn scope_error(provider: &str) -> Error {
    Error::Auth(format!(
        "Sign in to {provider} again (Settings → Accounts) to let Envelope change automatic replies."
    ))
}

fn is_consumer_gmail(email: &str) -> bool {
    let domain = email.rsplit('@').next().unwrap_or("").to_ascii_lowercase();
    domain == "gmail.com" || domain == "googlemail.com"
}

impl Engine {
    /// The account's automatic reply and what it supports.
    pub async fn auto_reply(&self, account_id: i64) -> Result<AutoReplyInfo> {
        let account = self.account_row(account_id)?;
        match account.kind {
            AccountKind::Microsoft => {
                use envelope_graph::{AutomaticRepliesStatus as S, ExternalAudienceScope as A};
                let client = crate::sync::calendar::graph_client(self, account_id)?;
                let current = client
                    .automatic_replies()
                    .await
                    .map_err(|e| graph_error(e, "Microsoft"))?;
                let (start, end) = if current.status == S::Scheduled {
                    days(
                        current
                            .scheduled_start_date_time
                            .as_ref()
                            .and_then(graph_instant),
                        current
                            .scheduled_end_date_time
                            .as_ref()
                            .and_then(graph_instant),
                    )
                } else {
                    (None, None)
                };
                let html = if current.internal_reply_message.trim().is_empty() {
                    &current.external_reply_message
                } else {
                    &current.internal_reply_message
                };
                Ok(AutoReplyInfo {
                    reply: AutoReply {
                        enabled: current.status != S::Disabled,
                        start,
                        end,
                        subject: String::new(),
                        message: html_to_text(html).trim().to_string(),
                        audience: match current.external_audience {
                            A::None => Audience::Organization,
                            A::ContactsOnly => Audience::Contacts,
                            A::All => Audience::Everyone,
                        },
                    },
                    audiences: vec![
                        Audience::Everyone,
                        Audience::Contacts,
                        Audience::Organization,
                    ],
                    subject: false,
                })
            }
            AccountKind::Exchange => {
                use envelope_ews::{ExternalAudience as A, OofState as S};
                let mail = crate::sync::ews::EwsMail::for_account(self, &account).await?;
                let current = mail
                    .client()
                    .oof_settings(&account.email)
                    .await
                    .map_err(crate::sync::ews::ews_error)?;
                let (start, end) = if current.state == S::Scheduled {
                    days(current.start, current.end)
                } else {
                    (None, None)
                };
                let html = if current.internal_reply.trim().is_empty() {
                    &current.external_reply
                } else {
                    &current.internal_reply
                };
                Ok(AutoReplyInfo {
                    reply: AutoReply {
                        enabled: current.state != S::Disabled,
                        start,
                        end,
                        subject: String::new(),
                        message: html_to_text(html).trim().to_string(),
                        audience: match current.external_audience {
                            A::None => Audience::Organization,
                            A::Known => Audience::Contacts,
                            A::All => Audience::Everyone,
                        },
                    },
                    audiences: vec![
                        Audience::Everyone,
                        Audience::Contacts,
                        Audience::Organization,
                    ],
                    subject: false,
                })
            }
            AccountKind::Gmail if account.config.auth == AuthMethod::OAuthGoogle => {
                let client = crate::sync::calendar::google_client(self, account_id)?;
                let current = client.vacation().await.map_err(google_error)?;
                let (start, end) = days(
                    current.start_time.and_then(DateTime::from_timestamp_millis),
                    current.end_time.and_then(DateTime::from_timestamp_millis),
                );
                let message = match (
                    &current.response_body_plain_text,
                    &current.response_body_html,
                ) {
                    (Some(text), _) if !text.trim().is_empty() => text.clone(),
                    (_, Some(html)) => html_to_text(html).trim().to_string(),
                    _ => String::new(),
                };
                let mut audiences = vec![Audience::Everyone, Audience::Contacts];
                if !is_consumer_gmail(&account.email) {
                    audiences.push(Audience::Organization);
                }
                Ok(AutoReplyInfo {
                    reply: AutoReply {
                        enabled: current.enable_auto_reply,
                        start,
                        end,
                        subject: current.response_subject.unwrap_or_default(),
                        message,
                        audience: if current.restrict_to_domain {
                            Audience::Organization
                        } else if current.restrict_to_contacts {
                            Audience::Contacts
                        } else {
                            Audience::Everyone
                        },
                    },
                    audiences,
                    subject: true,
                })
            }
            _ => {
                let password = self.sieve_password(&account).await?;
                let login = sieve_login(&account, &password)?;
                let kept = sieve::load(&login).await?.unwrap_or_default();
                Ok(AutoReplyInfo {
                    reply: AutoReply {
                        enabled: kept.enabled,
                        start: kept.start,
                        end: kept.end,
                        subject: kept.subject,
                        message: kept.message,
                        audience: Audience::Everyone,
                    },
                    audiences: vec![Audience::Everyone],
                    subject: true,
                })
            }
        }
    }

    /// Changes the account's automatic reply.
    pub async fn set_auto_reply(&self, account_id: i64, reply: AutoReply) -> Result<()> {
        let account = self.account_row(account_id)?;
        let period = period(&reply)?;
        if reply.enabled && reply.message.trim().is_empty() {
            return Err(Error::Invalid(
                "Write the message of the automatic reply.".into(),
            ));
        }
        match account.kind {
            AccountKind::Microsoft => {
                use envelope_graph::{
                    AutomaticRepliesStatus as S, DateTimeTimeZone, ExternalAudienceScope as A,
                };
                let client = crate::sync::calendar::graph_client(self, account_id)?;
                let mut setting = client
                    .automatic_replies()
                    .await
                    .map_err(|e| graph_error(e, "Microsoft"))?;
                let period = closed(period)?;
                setting.status = match (reply.enabled, period) {
                    (false, _) => S::Disabled,
                    (true, None) => S::AlwaysEnabled,
                    (true, Some(_)) => S::Scheduled,
                };
                if let Some((start, end)) = period {
                    setting.scheduled_start_date_time = Some(DateTimeTimeZone::utc(start));
                    setting.scheduled_end_date_time = Some(DateTimeTimeZone::utc(end));
                }
                setting.external_audience = match reply.audience {
                    Audience::Everyone => A::All,
                    Audience::Contacts => A::ContactsOnly,
                    Audience::Organization => A::None,
                };
                let html = html_for(&reply.message, &setting.internal_reply_message);
                setting.internal_reply_message = html.clone();
                setting.external_reply_message = html;
                client
                    .set_automatic_replies(&setting)
                    .await
                    .map_err(|e| graph_error(e, "Microsoft"))
            }
            AccountKind::Exchange => {
                use envelope_ews::{ExternalAudience as A, OofState as S};
                let mail = crate::sync::ews::EwsMail::for_account(self, &account).await?;
                let mut settings = mail
                    .client()
                    .oof_settings(&account.email)
                    .await
                    .map_err(crate::sync::ews::ews_error)?;
                let period = closed(period)?;
                settings.state = match (reply.enabled, period) {
                    (false, _) => S::Disabled,
                    (true, None) => S::Enabled,
                    (true, Some(_)) => S::Scheduled,
                };
                if let Some((start, end)) = period {
                    settings.start = Some(start);
                    settings.end = Some(end);
                }
                settings.external_audience = match reply.audience {
                    Audience::Everyone => A::All,
                    Audience::Contacts => A::Known,
                    Audience::Organization => A::None,
                };
                let html = html_for(&reply.message, &settings.internal_reply);
                settings.internal_reply = html.clone();
                settings.external_reply = html;
                mail.client()
                    .set_oof_settings(&account.email, &settings)
                    .await
                    .map_err(crate::sync::ews::ews_error)
            }
            AccountKind::Gmail if account.config.auth == AuthMethod::OAuthGoogle => {
                let client = crate::sync::calendar::google_client(self, account_id)?;
                let current = client.vacation().await.map_err(google_error)?;
                let html = html_for(
                    &reply.message,
                    current.response_body_html.as_deref().unwrap_or(""),
                );
                let settings = envelope_google::VacationSettings {
                    enable_auto_reply: reply.enabled,
                    response_subject: Some(reply.subject.trim().to_string())
                        .filter(|s| !s.is_empty()),
                    response_body_plain_text: Some(reply.message.clone()),
                    response_body_html: Some(html),
                    restrict_to_contacts: reply.audience == Audience::Contacts,
                    restrict_to_domain: reply.audience == Audience::Organization
                        && !is_consumer_gmail(&account.email),
                    start_time: period.0.map(|s| s.timestamp_millis()),
                    end_time: period.1.map(|e| e.timestamp_millis()),
                };
                client.set_vacation(&settings).await.map_err(google_error)?;
                Ok(())
            }
            _ => {
                let password = self.sieve_password(&account).await?;
                let login = sieve_login(&account, &password)?;
                let mut addresses = vec![account.email.clone()];
                for identity in self.identities()? {
                    if identity.account_id == account.id
                        && !addresses
                            .iter()
                            .any(|a| a.eq_ignore_ascii_case(&identity.email))
                    {
                        addresses.push(identity.email.clone());
                    }
                }
                let vacation = sieve::Vacation {
                    enabled: reply.enabled,
                    subject: reply.subject.trim().to_string(),
                    message: reply.message.trim_end().to_string(),
                    start: reply.start,
                    end: reply.end,
                    addresses,
                };
                sieve::save(&login, &vacation).await
            }
        }
    }

    async fn sieve_password(&self, account: &AccountRow) -> Result<String> {
        if account.config.auth != AuthMethod::Password || account.config.imap.is_none() {
            return Err(unsupported());
        }
        self.secrets
            .get(&crate::secrets::password_key(account.id))
            .await?
            .ok_or_else(|| Error::Auth("password missing, please sign in again".into()))
    }
}

fn sieve_login<'a>(account: &'a AccountRow, password: &'a str) -> Result<sieve::Login<'a>> {
    let imap = account.config.imap.as_ref().ok_or_else(unsupported)?;
    if NO_SIEVE.iter().any(|h| imap.host.eq_ignore_ascii_case(h)) {
        return Err(unsupported());
    }
    let local = matches!(imap.host.as_str(), "localhost" | "127.0.0.1" | "::1");
    Ok(sieve::Login {
        host: &imap.host,
        // The development server keeps its services on high ports.
        port: std::env::var("ENVELOPE_SIEVE_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(sieve::PORT),
        user: if imap.username.is_empty() {
            &account.email
        } else {
            &imap.username
        },
        password,
        accept_invalid_certs: account.config.accept_invalid_certs,
        allow_plain: local || imap.security == Security::None,
    })
}

fn graph_instant(value: &envelope_graph::DateTimeTimeZone) -> Option<DateTime<Utc>> {
    value.to_utc().or_else(|| {
        let tz = envelope_ical::resolve_tzid(value.time_zone.as_deref()?)?;
        let local = value.local()?;
        tz.from_local_datetime(&local)
            .earliest()
            .map(|t| t.with_timezone(&Utc))
    })
}

fn graph_error(e: envelope_graph::GraphError, provider: &str) -> Error {
    // 403: the grant predates the mailbox settings permission.
    if e.status() == Some(403) {
        scope_error(provider)
    } else {
        crate::sync::calendar::graph_err(e)
    }
}

fn google_error(e: envelope_google::Error) -> Error {
    if e.is_forbidden() {
        scope_error("Google")
    } else {
        crate::sync::calendar::google_err(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periods_cover_whole_days() {
        let reply = AutoReply {
            start: NaiveDate::from_ymd_opt(2026, 11, 2),
            end: NaiveDate::from_ymd_opt(2026, 11, 6),
            ..Default::default()
        };
        let (start, end) = closed(period(&reply).unwrap()).unwrap().unwrap();
        assert_eq!(end - start, Duration::days(5));
        assert_eq!(days(Some(start), Some(end)), (reply.start, reply.end));
        let backwards = AutoReply {
            start: reply.end,
            end: reply.start,
            ..Default::default()
        };
        assert!(period(&backwards).is_err());
        // Open-ended periods are fine where the service allows them.
        let open = AutoReply {
            start: reply.start,
            ..Default::default()
        };
        let (from, until) = period(&open).unwrap();
        assert_eq!((from.is_some(), until.is_none()), (true, true));
        assert!(closed((from, until)).is_err());
    }

    #[test]
    fn keeps_formatting_made_elsewhere() {
        let html = "<p>Away until <b>Monday</b>.</p>";
        assert_eq!(html_for("Away until Monday.", html), html);
        assert_eq!(
            html_for("Away until Tuesday.\nCall me.", html),
            "Away until Tuesday.<br>Call me."
        );
        assert_eq!(html_for("a < b", ""), "a &lt; b");
    }
}
