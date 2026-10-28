//! The Envelope desktop shell: wires the engine to the web UI.

mod autostart;
mod commands;
mod crypto;
mod notify;
mod protocol;
mod tray;

use envelope_core::model::EngineEvent;
use envelope_core::{Engine, EventSink, Paths};
use tauri::{AppHandle, Emitter, Manager};

pub struct AppState {
    pub engine: Engine,
    /// Started with `--minimized` (at login): stay in the tray until asked.
    pub start_hidden: std::sync::atomic::AtomicBool,
    pub ai_runs: parking_lot::Mutex<std::collections::HashMap<String, tokio::task::AbortHandle>>,
}

struct TauriSink {
    app: AppHandle,
}

impl EventSink for TauriSink {
    fn emit(&self, event: EngineEvent) {
        if let EngineEvent::NewMail {
            account_id,
            threads,
        } = &event
        {
            notify::new_mail(&self.app, *account_id, threads);
        }
        if let EngineEvent::Reminder {
            event_id,
            title,
            start,
            location,
        } = &event
        {
            notify::reminder(&self.app, *event_id, title, *start, location.as_deref());
        }
        if let EngineEvent::FollowUpDue { thread_id, subject } = &event
            && !subject.is_empty()
        {
            notify::follow_up(&self.app, *thread_id, subject);
        }
        let _ = self.app.emit("engine", &event);
    }
}

/// Handles `mailto:` and `webcal:` links, `--compose` and message or calendar
/// files passed on the command line.
fn handle_args(app: &AppHandle, args: &[String], cwd: &std::path::Path) {
    for arg in args.iter().skip(1) {
        if arg.starts_with("mailto:") {
            let _ = app.emit("mailto", arg.clone());
        } else if arg.starts_with("webcal:") || arg.starts_with("webcals:") {
            let _ = app.emit("webcal", arg.clone());
        } else if arg == "--compose" {
            let _ = app.emit("mailto", "mailto:");
        } else if let Some(path) = file_arg(arg, cwd) {
            let _ = app.emit("open-file", path.to_string_lossy());
        }
    }
}

/// A file named on the command line, as a path or a `file:` URL.
fn file_arg(arg: &str, cwd: &std::path::Path) -> Option<std::path::PathBuf> {
    if arg.starts_with('-') {
        return None;
    }
    let path = if arg.starts_with("file:") {
        url::Url::parse(arg).ok()?.to_file_path().ok()?
    } else {
        std::path::PathBuf::from(arg)
    };
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    path.is_file().then_some(path)
}

pub(crate) fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("ENVELOPE_LOG")
                .unwrap_or_else(|_| "warn,envelope_core=info,envelope=info".into()),
        )
        .init();

    // WebKitGTK's DMA-BUF renderer is broken on the NVIDIA proprietary driver.
    if std::path::Path::new("/proc/driver/nvidia/version").exists()
        && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none()
    {
        // SAFETY: set before any other thread is started.
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }

    let result = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            show_main(app);
            handle_args(app, &argv, std::path::Path::new(&cwd));
        }))
        // Size and position are restored; showing the window is left to
        // `app_ready`, so that it appears once the interface has loaded and
        // stays in the tray when started at login.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::all()
                        - tauri_plugin_window_state::StateFlags::VISIBLE,
                )
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .register_asynchronous_uri_scheme_protocol("att", protocol::handle)
        .setup(|app| {
            let handle = app.handle().clone();
            let engine = tauri::async_runtime::block_on(async {
                Engine::start(
                    Paths::standard(),
                    TauriSink {
                        app: handle.clone(),
                    },
                )
            })?;
            let minimized = std::env::args().any(|a| a == "--minimized") && tray::available();
            app.manage(AppState {
                engine,
                start_hidden: std::sync::atomic::AtomicBool::new(minimized),
                ai_runs: Default::default(),
            });
            // Respect the user's title bar preference before the window shows.
            if let Some(w) = app.get_webview_window("main") {
                let native = app
                    .state::<AppState>()
                    .engine
                    .get_setting("nativeTitlebar")
                    .ok()
                    .flatten()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let _ = w.set_decorations(native);
            }
            #[cfg(target_os = "linux")]
            if let Some(w) = app.get_webview_window("main") {
                enable_spell_checking(&w);
            }
            let args: Vec<String> = std::env::args().collect();
            let cwd = std::env::current_dir().unwrap_or_default();
            let pending = handle.clone();
            tauri::async_runtime::spawn(async move {
                // Give the UI a moment to subscribe before delivering links and files.
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                handle_args(&pending, &args, &cwd);
            });
            if let Err(e) = tray::create(app.handle()) {
                tracing::warn!("could not create the tray icon: {e}");
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // With "keep running in the background", closing only hides the window.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
                && tray::close_to_tray(window.app_handle())
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_ready,
            commands::app_info,
            commands::accounts,
            commands::discover,
            commands::account_add_password,
            commands::account_add_exchange,
            commands::account_add_oauth,
            commands::account_update,
            commands::account_remove,
            commands::accounts_reorder,
            commands::oauth_client_get,
            commands::oauth_client_set,
            commands::sync_now,
            commands::folders,
            commands::threads,
            commands::thread,
            commands::load_bodies,
            commands::set_flags,
            commands::mark_folder_read,
            commands::move_threads,
            commands::undo,
            commands::delete_forever,
            commands::snooze,
            commands::set_label,
            commands::message_source,
            commands::unsubscribe,
            commands::drafts,
            commands::draft,
            commands::save_draft,
            commands::delete_draft,
            commands::compose_reply,
            commands::send,
            commands::cancel_send,
            commands::retry_send,
            commands::outbox,
            commands::suggest_contacts,
            commands::identities,
            commands::identity_save,
            commands::identity_delete,
            commands::file_info,
            commands::stash_file,
            commands::attachment_open,
            commands::attachment_save,
            commands::attachment_reveal,
            commands::free_busy,
            commands::auto_reply_get,
            commands::auto_reply_set,
            commands::file_open,
            commands::file_import_events,
            commands::file_part_open,
            commands::file_part_save,
            commands::settings_get,
            commands::settings_set,
            commands::set_decorations,
            commands::set_unread_badge,
            commands::ai_config,
            commands::ai_set_config,
            commands::ai_presets,
            commands::ai_models,
            commands::ai_test,
            commands::ai_run,
            commands::ai_cancel,
            commands::calendars,
            commands::calendar_set_visible,
            commands::calendar_set_color,
            commands::calendar_subscribe,
            commands::calendar_refresh,
            commands::calendar_unsubscribe,
            commands::occurrences,
            commands::event_detail,
            commands::event_create,
            commands::event_update,
            commands::event_move,
            commands::event_delete,
            commands::event_respond,
            commands::invite_info,
            commands::invite_respond,
            commands::contact_books,
            commands::contacts_list,
            commands::contact_get,
            commands::contact_save,
            commands::contact_delete,
            commands::autostart_get,
            commands::autostart_set,
            crypto::crypto_settings_get,
            crypto::crypto_settings_set,
            crypto::crypto_own_keys,
            crypto::crypto_generate_key,
            crypto::crypto_import_text,
            crypto::crypto_import_file,
            crypto::crypto_export_public,
            crypto::crypto_export_secret,
            crypto::crypto_export_file,
            crypto::crypto_delete_key,
            crypto::crypto_keys,
            crypto::crypto_set_verified,
            crypto::crypto_set_own_key,
            crypto::crypto_unlock,
            crypto::crypto_lock,
            crypto::crypto_compose_status,
            crypto::crypto_lookup,
            crypto::crypto_open,
        ])
        .build(tauri::generate_context!());
    match result {
        Ok(app) => app.run(|app, event| {
            if let tauri::RunEvent::Exit = event
                && let Some(state) = app.try_state::<AppState>()
            {
                state.engine.shutdown();
            }
        }),
        Err(e) => {
            eprintln!("Envelope failed to start: {e}");
            std::process::exit(1);
        }
    }
}

/// WebKitGTK ships with spell checking off; turn it on for the user's languages.
#[cfg(target_os = "linux")]
fn enable_spell_checking(window: &tauri::WebviewWindow) {
    let languages = spell_languages();
    let _ = window.with_webview(move |webview| {
        use webkit2gtk::{WebContextExt, WebViewExt};
        if let Some(context) = webview.inner().context() {
            context.set_spell_checking_enabled(true);
            let refs: Vec<&str> = languages.iter().map(String::as_str).collect();
            context.set_spell_checking_languages(&refs);
        }
    });
}

/// Dictionary names (`tr_TR`, `en_US`, …) from the locale environment, English last.
#[cfg(target_os = "linux")]
fn spell_languages() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |tag: &str| {
        let tag = tag.split(['.', '@']).next().unwrap_or("").trim();
        if !tag.is_empty() && tag != "C" && tag != "POSIX" && !out.iter().any(|t| t == tag) {
            out.push(tag.to_string());
        }
    };
    if let Ok(list) = std::env::var("LANGUAGE") {
        list.split(':').for_each(&mut push);
    }
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(var) {
            push(&v);
        }
    }
    push("en_US");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn finds_files_named_on_the_command_line() {
        let dir = std::env::temp_dir().join(format!("envelope-args-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub dir")).unwrap();
        let file = dir.join("sub dir").join("invite.ics");
        std::fs::write(&file, "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").unwrap();

        assert_eq!(file_arg("sub dir/invite.ics", &dir), Some(file.clone()));
        assert_eq!(
            file_arg(file.to_str().unwrap(), Path::new("/")),
            Some(file.clone())
        );
        let url = url::Url::from_file_path(&file).unwrap().to_string();
        assert!(url.contains("sub%20dir"), "{url}");
        assert_eq!(file_arg(&url, Path::new("/")), Some(file.clone()));
        assert_eq!(file_arg("--minimized", &dir), None);
        assert_eq!(file_arg("missing.eml", &dir), None);
        assert_eq!(file_arg("sub dir", &dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
