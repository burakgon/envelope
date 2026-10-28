//! The outbox: delayed (undo window), scheduled and offline sending.

use std::time::Duration;

use crate::db::misc::{self, OutboxRow, PendingOp};
use crate::db::{mail, now_ms};
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::*;
use crate::smtp::{self, SmtpAuth};

/// Providers whose SMTP servers file sent mail into the Sent folder themselves.
const AUTO_SAVES_SENT: &[&str] = &["gmail", "outlook", "yahoo", "aol"];

pub(crate) fn spawn_scheduler(engine: Engine) {
    tokio::spawn(async move {
        // Messages interrupted mid-send (crash, power loss) go back to the queue.
        let _ = engine.db.write(|tx| {
            tx.execute(
                "UPDATE outbox SET state = 'queued' WHERE state = 'sending'",
                [],
            )?;
            Ok(())
        });
        loop {
            let due = engine
                .db
                .read(|c| misc::outbox_due(c, now_ms()))
                .unwrap_or_default();
            for row in due {
                send_one(&engine, row).await;
            }
            let next = engine.db.read(misc::outbox_next_time).ok().flatten();
            let wait = next
                .map(|t| Duration::from_millis((t - now_ms()).clamp(50, 60_000) as u64))
                .unwrap_or(Duration::from_secs(60));
            tokio::select! {
                _ = engine.shutdown.cancelled() => return,
                _ = engine.outbox_wake.notified() => {}
                _ = tokio::time::sleep(wait) => {}
            }
        }
    });
}

async fn smtp_auth(
    engine: &Engine,
    account: &mail::AccountRow,
    server: &ServerConfig,
    force: bool,
) -> Result<SmtpAuth> {
    let user = if server.username.is_empty() {
        account.email.clone()
    } else {
        server.username.clone()
    };
    match account.config.auth {
        AuthMethod::Password => Ok(SmtpAuth::Password {
            user,
            password: engine
                .secrets
                .get(&crate::secrets::password_key(account.id))
                .await?
                .ok_or_else(|| Error::Auth("password missing".into()))?,
        }),
        _ => Ok(SmtpAuth::XOAuth2 {
            user: account.email.clone(),
            token: engine.tokens.access_token(account.id, force).await?,
        }),
    }
}

async fn deliver(engine: &Engine, row: &OutboxRow) -> Result<()> {
    let account = engine.account_row(row.account_id)?;
    match account.kind {
        AccountKind::Imap | AccountKind::Gmail => {
            let server = account
                .config
                .smtp
                .clone()
                .ok_or_else(|| Error::Invalid("no outgoing (SMTP) server configured".into()))?;
            let auth = smtp_auth(engine, &account, &server, false).await?;
            let result = smtp::send(
                &server,
                &auth,
                account.config.accept_invalid_certs,
                &row.meta.from,
                &row.meta.recipients,
                &row.mime,
            )
            .await;
            match result {
                Err(e) if e.is_auth() && account.config.auth != AuthMethod::Password => {
                    let auth = smtp_auth(engine, &account, &server, true).await?;
                    smtp::send(
                        &server,
                        &auth,
                        account.config.accept_invalid_certs,
                        &row.meta.from,
                        &row.meta.recipients,
                        &row.mime,
                    )
                    .await
                }
                other => other,
            }
        }
        AccountKind::Microsoft => {
            crate::sync::graph::send(engine, account.id, &row.mime, &row.meta.recipients).await
        }
    }
}

async fn send_one(engine: &Engine, row: OutboxRow) {
    let id = row.id;
    let _ = engine
        .db
        .write(|tx| misc::outbox_set_state(tx, id, "sending", None));
    engine.emit(EngineEvent::OutboxChanged);
    match deliver(engine, &row).await {
        Ok(()) => {
            if let Err(e) = after_sent(engine, &row) {
                tracing::warn!("post-send bookkeeping failed: {e}");
            }
            engine.emit(EngineEvent::OutboxChanged);
            engine.wake(row.account_id);
        }
        Err(e) => {
            tracing::warn!("sending failed: {e}");
            let transient = e.is_transient();
            let _ = engine.db.write(|tx| {
                if transient && row.attempts < 8 {
                    let delay = 30_000 * (1 << row.attempts.min(5));
                    misc::outbox_set_state(tx, id, "queued", Some(&e.to_string()))?;
                    misc::outbox_reschedule(tx, id, now_ms() + delay)?;
                    tx.execute(
                        "UPDATE outbox SET attempts = attempts + 1 WHERE id = ?",
                        [id],
                    )?;
                } else {
                    misc::outbox_set_state(tx, id, "failed", Some(&e.to_string()))?;
                }
                Ok(())
            });
            engine.emit(EngineEvent::SendFailed {
                outbox_id: id,
                error: e.to_string(),
            });
            engine.emit(EngineEvent::OutboxChanged);
        }
    }
}

fn after_sent(engine: &Engine, row: &OutboxRow) -> Result<()> {
    let account = engine.account_row(row.account_id)?;
    let save_copy = account.config.save_sent_copy.unwrap_or(
        !AUTO_SAVES_SENT.contains(&account.provider.as_str()) && account.kind == AccountKind::Imap,
    );
    engine.db.write(|tx| {
        if save_copy {
            tx.execute("UPDATE outbox SET state = 'sent' WHERE id = ?", [row.id])?;
            misc::queue_op(
                tx,
                row.account_id,
                &PendingOp::AppendSent { outbox_id: row.id },
                0,
            )?;
        } else {
            misc::outbox_delete(tx, row.id)?;
        }
        if let Some(draft_id) = row.draft_id
            && let Some(draft) = misc::draft_delete(tx, draft_id)?
            && (draft.remote_uid.is_some() || draft.remote_id.is_some())
        {
            misc::queue_op(
                tx,
                draft.account_id,
                &PendingOp::DeleteDraft {
                    remote_uid: draft.remote_uid,
                    remote_id: draft.remote_id,
                },
                0,
            )?;
        }
        let replied = row.meta.reply_to_message_id.map(|id| (id, "\\Answered"));
        let forwarded = row.meta.forward_of.map(|id| (id, "$Forwarded"));
        for (message_id, flag) in replied.into_iter().chain(forwarded) {
            let bit = if flag == "\\Answered" {
                Flags::ANSWERED
            } else {
                Flags::FORWARDED
            };
            tx.execute(
                "UPDATE messages SET flags = flags | ? WHERE id = ?",
                rusqlite::params![bit, message_id],
            )?;
            misc::queue_op(
                tx,
                row.account_id,
                &PendingOp::Flags {
                    message_ids: vec![message_id],
                    add: vec![flag.to_string()],
                    remove: vec![],
                },
                0,
            )?;
        }
        misc::record_addresses(tx, &row.meta.to, true, now_ms())?;
        Ok(())
    })
}
