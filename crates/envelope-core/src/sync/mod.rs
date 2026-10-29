//! Background synchronization: per-account workers and schedulers.

pub mod calendar;
pub mod contacts;
pub mod ews;
pub mod graph;
mod identities;
pub mod imap;
pub mod outbox;
mod subscriptions;
pub mod tasks;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::compose::{self, FilePart, Outgoing, Rendered};
use crate::db::mail;
use crate::db::misc;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::*;

/// Runs the right sync implementation for an account.
pub(crate) async fn run_account(
    engine: Engine,
    account_id: i64,
    wake: Arc<Notify>,
    stop: CancellationToken,
) {
    let Ok(account) = engine.account_row(account_id) else {
        return;
    };
    if account.kind == AccountKind::Gmail && account.config.auth == AuthMethod::OAuthGoogle {
        let engine = engine.clone();
        let account = account.clone();
        tokio::spawn(async move {
            // Grants from before the settings permission cannot list aliases.
            if let Err(e) = identities::import_gmail_aliases(&engine, &account).await {
                tracing::debug!(account = account.id, "Gmail aliases not imported: {e}");
            }
        });
    }
    match account.kind {
        AccountKind::Imap | AccountKind::Gmail => imap::run(engine, account_id, wake, stop).await,
        AccountKind::Microsoft => graph::run(engine, account_id, wake, stop).await,
        AccountKind::Exchange => ews::run(engine, account_id, wake, stop).await,
    }
}

/// Why the most recent attempt to obtain an access token failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenFailure {
    /// The grant was rejected or is missing: the user has to sign in again.
    Auth,
    /// The identity provider could not be reached.
    Network,
    /// Anything else, such as an unavailable keyring.
    Other,
}

/// OAuth access tokens for the protocol crates.
pub(crate) struct AccountTokens {
    engine: Engine,
    account_id: i64,
    stale: AtomicBool,
    failure: parking_lot::Mutex<Option<TokenFailure>>,
}

impl AccountTokens {
    pub(crate) fn new(engine: &Engine, account_id: i64) -> Arc<AccountTokens> {
        Arc::new(AccountTokens {
            engine: engine.clone(),
            account_id,
            stale: AtomicBool::new(false),
            failure: parking_lot::Mutex::new(None),
        })
    }

    async fn token(&self) -> std::result::Result<String, String> {
        let force = self.stale.swap(false, Ordering::SeqCst);
        let result = self
            .engine
            .tokens
            .access_token(self.account_id, force)
            .await;
        *self.failure.lock() = result.as_ref().err().map(|e| {
            if e.is_auth() {
                TokenFailure::Auth
            } else if e.is_transient() {
                TokenFailure::Network
            } else {
                TokenFailure::Other
            }
        });
        result.map_err(|e| e.to_string())
    }

    /// Why the most recent token request failed, if it did. Protocol crates
    /// only pass the error text along; this tells "sign in again" apart from
    /// "offline".
    pub(crate) fn last_failure(&self) -> Option<TokenFailure> {
        *self.failure.lock()
    }
}

#[async_trait::async_trait]
impl envelope_dav::TokenSource for AccountTokens {
    async fn access_token(&self) -> std::result::Result<String, String> {
        self.token().await
    }
    async fn invalidate(&self) {
        self.stale.store(true, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl envelope_google::TokenSource for AccountTokens {
    async fn access_token(&self) -> std::result::Result<String, String> {
        self.token().await
    }
    async fn invalidate(&self) {
        self.stale.store(true, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl envelope_graph::TokenSource for AccountTokens {
    async fn access_token(&self) -> std::result::Result<String, String> {
        self.token().await
    }
    async fn invalidate(&self) {
        self.stale.store(true, Ordering::SeqCst);
    }
}

/// Periodic local chores: snoozed threads coming back.
pub(crate) fn spawn_housekeeping(engine: Engine) {
    subscriptions::spawn(engine.clone());
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = engine.shutdown.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(20)) => {}
            }
            check_followups(&engine);
            let now = crate::db::now_ms();
            let due = match engine.db.read(|c| mail::due_snoozes(c, now)) {
                Ok(d) => d,
                Err(_) => continue,
            };
            if due.is_empty() {
                continue;
            }
            let ids = due.clone();
            if let Ok(folders) = engine.db.write(|tx| mail::set_snooze(tx, &ids, None)) {
                let summaries = engine
                    .db
                    .read(|c| mail::thread_summaries(c, &due))
                    .unwrap_or_default();
                let mut by_account: std::collections::HashMap<i64, Vec<ThreadSummary>> =
                    Default::default();
                for s in summaries {
                    by_account.entry(s.account_id).or_default().push(s);
                }
                for (account_id, threads) in by_account {
                    engine.emit(EngineEvent::MailChanged {
                        account_id,
                        folder_ids: folders.iter().copied().collect(),
                        thread_ids: threads.iter().map(|t| t.id).collect(),
                    });
                    engine.emit(EngineEvent::NewMail {
                        account_id,
                        threads,
                    });
                }
            }
        }
    });
}

/// Follow-up reminders: tells the user about sent messages nobody answered in
/// time, and stops watching once someone replies.
fn check_followups(engine: &Engine) {
    let now = crate::db::now_ms();
    let Ok(list) = engine.db.read(|c| misc::followups_to_check(c, now)) else {
        return;
    };
    for f in list {
        // The sent copy hasn't synced yet: look again later.
        let Some(thread_id) = f.thread_id else {
            continue;
        };
        if f.replied {
            let _ = engine
                .db
                .write(|tx| misc::followup_remove(tx, &f.message_id));
            if f.notified {
                engine.emit(EngineEvent::FollowUpDue {
                    thread_id,
                    subject: String::new(),
                });
            }
        } else if !f.notified
            && engine
                .db
                .write(|tx| misc::followup_mark_notified(tx, &f.message_id))
                .is_ok()
        {
            engine.emit(EngineEvent::FollowUpDue {
                thread_id,
                subject: f.subject,
            });
        }
    }
}

/// Sender identity for a draft.
pub(crate) fn draft_identity(
    engine: &Engine,
    draft: &DraftInput,
) -> Result<(Address, Option<Address>)> {
    let identities = engine.db.read(misc::list_identities)?;
    let identity = draft
        .identity_id
        .and_then(|id| identities.iter().find(|i| i.id == id))
        .or_else(|| {
            identities
                .iter()
                .find(|i| i.account_id == draft.account_id && i.is_default)
        })
        .or_else(|| identities.iter().find(|i| i.account_id == draft.account_id));
    match (&draft.from, identity) {
        (Some(from), _) if !from.email.is_empty() => Ok((from.clone(), None)),
        (_, Some(i)) => Ok((
            Address::new(Some(i.name.clone()), i.email.clone()),
            i.reply_to
                .as_ref()
                .filter(|r| !r.is_empty())
                .map(|r| Address::new(None, r.clone())),
        )),
        _ => {
            let account = engine.account_row(draft.account_id)?;
            Ok((Address::new(Some(account.name), account.email), None))
        }
    }
}

/// Renders a draft into MIME, loading attachments from disk or the server.
pub(crate) async fn render_draft(
    engine: &Engine,
    draft: &DraftInput,
    for_draft: bool,
) -> Result<Rendered> {
    let (from, reply_to) = draft_identity(engine, draft)?;
    let (in_reply_to, references) = match draft.reply_to_message_id {
        Some(id) if draft.mode.as_deref() != Some("forward") => {
            match engine.db.read(|c| mail::get_message(c, id)) {
                Ok(m) => (m.message_id.clone(), m.references.clone()),
                Err(_) => (None, Vec::new()),
            }
        }
        _ => (None, Vec::new()),
    };
    let mut attachments = Vec::new();
    for a in &draft.attachments {
        let data = if let Some(path) = &a.path {
            tokio::fs::read(path)
                .await
                .map_err(|e| Error::Invalid(format!("cannot read {}: {e}", a.filename)))?
        } else if let Some(id) = a.attachment_id {
            let path = engine.attachment_file(id).await?;
            tokio::fs::read(path).await?
        } else {
            continue;
        };
        attachments.push(FilePart {
            filename: a.filename.clone(),
            mime_type: a.mime_type.clone(),
            data,
            content_id: if a.inline { a.content_id.clone() } else { None },
        });
    }
    let mut out = Outgoing {
        from,
        reply_to,
        to: draft.to.clone(),
        cc: draft.cc.clone(),
        bcc: draft.bcc.clone(),
        subject: draft.subject.clone(),
        html: draft.html.clone(),
        text: draft.text.clone(),
        in_reply_to,
        references,
        attachments,
        read_receipt: draft.request_read_receipt,
        allow_no_recipients: for_draft,
        ..Default::default()
    };
    // OpenPGP: the sender's key in an Autocrypt header, encryption and
    // signatures as the composer asked.
    let plan = crate::crypto::plan_outgoing(engine, draft, &out.from, for_draft).await?;
    if let Some(header) = &plan.autocrypt {
        out.extra_headers
            .push(("Autocrypt".to_string(), header.clone()));
    }
    crate::crypto::apply_plan(&plan, compose::render(&out)?)
}
