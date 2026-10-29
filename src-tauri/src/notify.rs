//! Desktop notifications (freedesktop.org) with click-to-open.

use envelope_core::model::{FlagChange, ThreadSummary};
use tauri::{AppHandle, Emitter, Manager};

use crate::AppState;
use crate::strings;

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
    show_with(app, summary, body, Vec::new(), None, move |app, _| {
        on_click(app)
    });
}

/// Shows a notification with extra buttons (`(action id, label)`) and, if
/// given, a sound of the desktop's sound theme. Clicking the notification
/// itself brings the window up and reports `"default"`; the buttons act
/// without opening the window.
fn show_with(
    app: &AppHandle,
    summary: String,
    body: String,
    buttons: Vec<(&'static str, String)>,
    sound: Option<&'static str>,
    on_action: impl FnOnce(&AppHandle, &str) + Send + 'static,
) {
    let app = app.clone();
    std::thread::spawn(move || {
        let mut n = notify_rust::Notification::new();
        n.appname("Envelope")
            .summary(&summary)
            .body(&body)
            .icon("io.github.burakgon.Envelope")
            .hint(notify_rust::Hint::DesktopEntry(
                "io.github.burakgon.Envelope".into(),
            ))
            .action("default", &strings::get("native.notify.open", "Open"));
        if let Some(sound) = sound {
            n.sound_name(sound);
        }
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
            strings::fill(
                "native.notify.newMessages",
                "{count} new messages",
                &[("count", &threads.len().to_string())],
            ),
            threads
                .iter()
                .take(4)
                .map(|t| format!("• {}", t.subject))
                .collect::<Vec<_>>()
                .join("\n"),
            None,
        )
    };
    let buttons = if thread.is_some() {
        vec![
            ("archive", strings::get("native.notify.archive", "Archive")),
            (
                "read",
                strings::get("native.notify.markRead", "Mark as Read"),
            ),
        ]
    } else {
        Vec::new()
    };
    // The sound theme's new-mail sound, unless turned off in Settings.
    let sound = enabled(app, "notificationSound").then_some("message-new-email");
    show_with(app, summary, body, buttons, sound, move |app, action| {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let result = match (action, thread) {
            ("archive", Some(id)) => state.engine.archive_from_inbox(&[id]).map(|_| ()),
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
        strings::get("native.notify.noReply", "No reply yet"),
        format!(
            "{subject}\n{}",
            strings::get("native.notify.followUp", "Follow up on this conversation?")
        ),
        move |app| {
            let _ = app.emit("open-thread", Some(thread_id));
        },
    );
}

/// A task is due at this time of day. Clicking opens it; the button
/// completes it without opening the window.
pub fn task_due(app: &AppHandle, task_id: i64, title: &str, due_at: i64) {
    if !enabled(app, "taskNotifications") {
        return;
    }
    let late = (now_ms() - due_at) / 60_000;
    let body = if late < 1 {
        strings::get("native.task.dueNow", "Due now")
    } else {
        strings::fill(
            "native.task.overdue",
            "Overdue by {minutes} min",
            &[("minutes", &late.to_string())],
        )
    };
    let buttons = vec![(
        "complete",
        strings::get("native.task.complete", "Mark as Done"),
    )];
    show_with(
        app,
        title.to_string(),
        body,
        buttons,
        None,
        move |app, action| {
            if action != "complete" {
                let _ = app.emit("open-task", task_id);
                return;
            }
            let Some(state) = app.try_state::<AppState>() else {
                return;
            };
            let engine = state.engine.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = engine.complete_task(task_id, true).await {
                    tracing::warn!("completing task {task_id} from its notification failed: {e}");
                }
            });
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn chrono_like(start_ms: i64) -> String {
    let mins = (start_ms - now_ms()) / 60_000;
    match mins {
        m if m <= 0 => strings::get("native.reminder.now", "Starting now"),
        1 => strings::get("native.reminder.inOneMinute", "In 1 minute"),
        m if m < 60 => strings::fill(
            "native.reminder.inMinutes",
            "In {minutes} minutes",
            &[("minutes", &m.to_string())],
        ),
        m if m % 60 == 0 => strings::fill(
            "native.reminder.inWholeHours",
            "In {hours} h",
            &[("hours", &(m / 60).to_string())],
        ),
        m => strings::fill(
            "native.reminder.inHours",
            "In {hours} h {minutes} min",
            &[
                ("hours", &(m / 60).to_string()),
                ("minutes", &(m % 60).to_string()),
            ],
        ),
    }
}
