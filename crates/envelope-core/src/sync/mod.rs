//! Background synchronization: per-account workers and schedulers.

pub mod imap;
pub mod outbox;

use std::sync::Arc;
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
    match account.kind {
        AccountKind::Imap | AccountKind::Gmail => imap::run(engine, account_id, wake, stop).await,
        AccountKind::Microsoft => {
            engine.set_sync_state(
                account_id,
                SyncState::Error {
                    message: "Microsoft accounts are not supported in this build yet".into(),
                },
            );
        }
    }
}

/// Periodic local chores: snoozed threads coming back.
pub(crate) fn spawn_housekeeping(engine: Engine) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = engine.shutdown.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(20)) => {}
            }
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

/// Sender identity for a draft.
pub(crate) fn draft_identity(
    engine: &Engine,
    draft: &DraftInput,
) -> Result<(Address, Option<Address>)> {
    let identities = engine.db.read(|c| misc::list_identities(c))?;
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
    let out = Outgoing {
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
    compose::render(&out)
}
