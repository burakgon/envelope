//! Desktop notifications (freedesktop.org) with click-to-open.

use envelope_core::model::{FlagChange, MoveTarget, ThreadSummary};
use tauri::{AppHandle, Emitter, Manager};

use crate::AppState;

fn enabled(app: &AppHandle, key: &str) -> bool {
    app.try_state::<AppState>()
        .and_then(|s| s.engine.get_setting(key).ok().flatten())
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

/// Whether the user is looking at the main window. A window hidden in the
/// tray or minimized can still report focus, depending on the window
/// manager, so those count as not looked at.
fn main_focused(app: &AppHandle) -> bool {
    app.get_webview_window("main").is_some_and(|w| {
        w.is_visible().unwrap_or(false)
            && !w.is_minimized().unwrap_or(false)
            && w.is_focused().unwrap_or(false)
    })
}

fn show(
    app: &AppHandle,
    summary: String,
    body: String,
    on_click: impl FnOnce(&AppHandle) + Send + 'static,
) {
    show_with(app, summary, body, &[], move |app, _| on_click(app));
}

/// Shows a notification with extra buttons (`(action id, label)`). Clicking
/// the notification itself brings the window up and reports `"default"`;
/// the buttons act without opening the window.
fn show_with(
    app: &AppHandle,
    summary: String,
    body: String,
    buttons: &[(&'static str, &'static str)],
    on_action: impl FnOnce(&AppHandle, &str) + Send + 'static,
) {
    let app = app.clone();
    let buttons = buttons.to_vec();
    std::thread::spawn(move || {
        let mut n = notify_rust::Notification::new();
        n.appname("Envelope")
            .summary(&summary)
            .body(&body)
            .icon("io.github.burakgon.Envelope")
            .hint(notify_rust::Hint::DesktopEntry(
                "io.github.burakgon.Envelope".into(),
            ))
            .action("default", "Open");
        for (id, label) in &buttons {
            n.action(id, label);
        }
        if let Ok(handle) = n.show() {
            handle.wait_for_action(|action| {
                if action == "default" {
                    if let Some(w) = app.get_webview_window("main") {
                        let _ = w.unminimize();
                        let _ = w.show();
                        let _ = w.set_focus();
                    }
                    on_action(&app, action);
                } else if buttons.iter().any(|(id, _)| *id == action) {
                    on_action(&app, action);
                }
            });
        }
    });
}

pub fn new_mail(app: &AppHandle, account_id: i64, threads: &[ThreadSummary]) {
    let focused = main_focused(app);
    tracing::debug!(account_id, threads = threads.len(), focused, "new mail");
    if !enabled(app, "notifications") || focused || threads.is_empty() {
        return;
    }
    let (summary, body, thread) = if threads.len() == 1 {
        let t = &threads[0];
        let from = t
            .participants
            .last()
            .map(|p| p.name.clone().unwrap_or_else(|| p.email.clone()))
            .unwrap_or_default();
        let body = if t.snippet.trim().is_empty() {
            t.subject.clone()
        } else {
            format!("{}\n{}", t.subject, t.snippet)
        };
        (from, body, Some(t.id))
    } else {
        (
            format!("{} new messages", threads.len()),
            threads
                .iter()
                .take(4)
                .map(|t| format!("• {}", t.subject))
                .collect::<Vec<_>>()
                .join("\n"),
            None,
        )
    };
    let buttons: &[(&str, &str)] = if thread.is_some() {
        &[("archive", "Archive"), ("read", "Mark as Read")]
    } else {
        &[]
    };
    show_with(app, summary, body, buttons, move |app, action| {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let result = match (action, thread) {
            ("archive", Some(id)) => state
                .engine
                .move_threads(&[id], None, &MoveTarget::Archive)
                .map(|_| ()),
            ("read", Some(id)) => state.engine.set_flags(
                &[id],
                &[],
                &FlagChange {
                    seen: Some(true),
                    flagged: None,
                },
            ),
            _ => {
                let _ = app.emit("open-thread", thread);
                Ok(())
            }
        };
        if let Err(e) = result {
            tracing::warn!("notification action {action} failed: {e}");
        }
    });
}

/// A sent message got no reply before its follow-up reminder.
pub fn follow_up(app: &AppHandle, thread_id: i64, subject: &str) {
    if !enabled(app, "notifications") {
        return;
    }
    show(
        app,
        "No reply yet".to_string(),
        format!("{subject}\nFollow up on this conversation?"),
        move |app| {
            let _ = app.emit("open-thread", Some(thread_id));
        },
    );
}

pub fn reminder(app: &AppHandle, event_id: i64, title: &str, start: i64, location: Option<&str>) {
    if !enabled(app, "calendarNotifications") {
        return;
    }
    let when = chrono_like(start);
    let body = match location {
        Some(l) if !l.is_empty() => format!("{when} · {l}"),
        _ => when,
    };
    show(app, title.to_string(), body, move |app| {
        let _ = app.emit("open-event", event_id);
    });
}

fn chrono_like(start_ms: i64) -> String {
    let mins = (start_ms
        - std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0))
        / 60_000;
    match mins {
        m if m <= 0 => "Starting now".into(),
        1 => "In 1 minute".into(),
        m if m < 60 => format!("In {m} minutes"),
        m => format!("In {} h {} min", m / 60, m % 60),
    }
}
