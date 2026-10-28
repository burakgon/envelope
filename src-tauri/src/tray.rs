//! System tray icon and running in the background.

use std::sync::OnceLock;

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

use crate::AppState;

const TRAY_ID: &str = "main";

/// Whether a tray icon can be shown. On Linux it needs an AppIndicator
/// library at run time; without one, creating the icon would abort the app.
pub fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        #[cfg(target_os = "linux")]
        {
            ["libayatana-appindicator3.so.1", "libappindicator3.so.1"]
                .iter()
                // SAFETY: only probes whether the library can be loaded; it is
                // unloaded again right away and no symbols are used.
                .any(|lib| unsafe { libloading::Library::new(lib) }.is_ok())
        }
        #[cfg(not(target_os = "linux"))]
        {
            true
        }
    })
}

/// Adds the tray icon with its menu.
pub fn create(app: &AppHandle) -> tauri::Result<()> {
    if !available() {
        tracing::info!("no AppIndicator library found; running without a tray icon");
        return Ok(());
    }
    let open = MenuItem::with_id(app, "open", "Open Envelope", true, None::<&str>)?;
    let compose = MenuItem::with_id(app, "compose", "New Message", true, None::<&str>)?;
    let sync = MenuItem::with_id(app, "sync", "Check for New Mail", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Envelope", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &compose, &sync, &separator, &quit])?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(include_bytes!("../icons/64x64.png"))?)
        .tooltip("Envelope")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => crate::show_main(app),
            "compose" => {
                crate::show_main(app);
                let _ = app.emit("mailto", "mailto:");
            }
            "sync" => {
                if let Some(state) = app.try_state::<AppState>() {
                    state.engine.sync_now(None);
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                crate::show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Shows the unread count on the tray icon.
pub fn set_unread(app: &AppHandle, count: i64) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let tooltip = match count {
        0 => "Envelope".to_string(),
        1 => "Envelope — 1 unread message".to_string(),
        n => format!("Envelope — {n} unread messages"),
    };
    let _ = tray.set_tooltip(Some(tooltip));
    // Panels that support it show this label next to the icon.
    let _ = tray.set_title((count > 0).then(|| count.to_string()));
}

/// Whether closing the window should keep Envelope running in the tray.
pub fn close_to_tray(app: &AppHandle) -> bool {
    available()
        && app
            .try_state::<AppState>()
            .and_then(|s| s.engine.get_setting("closeToTray").ok().flatten())
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
}
