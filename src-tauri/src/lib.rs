//! The Envelope desktop shell: wires the engine to the web UI.

mod commands;
mod notify;
mod protocol;

use envelope_core::model::EngineEvent;
use envelope_core::{Engine, EventSink, Paths};
use tauri::{AppHandle, Emitter, Manager};

pub struct AppState {
    pub engine: Engine,
    pub ai_runs: parking_lot::Mutex<std::collections::HashMap<String, tokio::task::AbortHandle>>,
}

struct TauriSink {
    app: AppHandle,
}

impl EventSink for TauriSink {
    fn emit(&self, event: EngineEvent) {
        if let EngineEvent::NewMail { account_id, threads } = &event {
            notify::new_mail(&self.app, *account_id, threads);
        }
        if let EngineEvent::Reminder { event_id, title, start, location } = &event {
            notify::reminder(&self.app, *event_id, title, *start, location.as_deref());
        }
        let _ = self.app.emit("engine", &event);
    }
}

/// Handles `mailto:` links and `--compose` passed on the command line.
fn handle_args(app: &AppHandle, args: &[String]) {
    for arg in args.iter().skip(1) {
        if arg.starts_with("mailto:") {
            let _ = app.emit("mailto", arg.clone());
        } else if arg == "--compose" {
            let _ = app.emit("mailto", "mailto:");
        }
    }
}

fn show_main(app: &AppHandle) {
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
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            show_main(app);
            handle_args(app, &argv);
        }))
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .register_asynchronous_uri_scheme_protocol("att", protocol::handle)
        .setup(|app| {
            let handle = app.handle().clone();
            let engine = tauri::async_runtime::block_on(async {
                Engine::start(Paths::standard(), TauriSink { app: handle.clone() })
            })?;
            app.manage(AppState { engine, ai_runs: Default::default() });
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
            let args: Vec<String> = std::env::args().collect();
            let pending = handle.clone();
            tauri::async_runtime::spawn(async move {
                // Give the UI a moment to subscribe before delivering mailto links.
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                handle_args(&pending, &args);
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event
                && window.label() == "main"
                && let Some(state) = window.app_handle().try_state::<AppState>()
            {
                state.engine.shutdown();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_ready,
            commands::app_info,
            commands::accounts,
            commands::discover,
            commands::account_add_password,
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
        ])
        .run(tauri::generate_context!());
    if let Err(e) = result {
        eprintln!("Envelope failed to start: {e}");
        std::process::exit(1);
    }
}
