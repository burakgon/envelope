//! System tray icon and running in the background.

use std::sync::OnceLock;

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

use crate::AppState;
use crate::strings;

const TRAY_ID: &str = "main";

/// Menu entries: id, translation key, English text.
const LABELS: &[(&str, &str, &str)] = &[
    ("open", "native.tray.open", "Open Envelope"),
    ("compose", "native.tray.compose", "New Message"),
    ("sync", "native.tray.sync", "Check for New Mail"),
    ("quit", "native.tray.quit", "Quit Envelope"),
];

/// The menu entries, relabeled when the interface language is known.
static ITEMS: OnceLock<Vec<MenuItem<tauri::Wry>>> = OnceLock::new();

/// Puts the current translations on the tray menu.
pub fn relabel() {
    for item in ITEMS.get().into_iter().flatten() {
        if let Some((_, key, english)) = LABELS.iter().find(|(id, _, _)| *id == item.id().as_ref())
        {
            let _ = item.set_text(strings::get(key, english));
        }
    }
}

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
    let item = |id: &str| -> tauri::Result<MenuItem<tauri::Wry>> {
        let (key, english) = LABELS
            .iter()
            .find(|(i, _, _)| *i == id)
            .map(|(_, k, e)| (*k, *e))
            .unwrap_or(("", id));
        MenuItem::with_id(app, id, strings::get(key, english), true, None::<&str>)
    };
    let (open, compose, sync, quit) = (
        item("open")?,
        item("compose")?,
        item("sync")?,
        item("quit")?,
    );
    let _ = ITEMS.set(vec![
        open.clone(),
        compose.clone(),
        sync.clone(),
        quit.clone(),
    ]);
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &compose, &sync, &separator, &quit])?;
    let mut tray = TrayIconBuilder::with_id(TRAY_ID);
    // The icon is handed to the panel as a file; inside Flatpak only this
    // runtime directory is visible to the host as well.
    if let (Some(runtime), Some(id)) = (
        std::env::var_os("XDG_RUNTIME_DIR"),
        std::env::var_os("FLATPAK_ID"),
    ) {
        tray = tray.temp_dir_path(
            std::path::Path::new(&runtime)
                .join("app")
                .join(id)
                .join("tray-icon"),
        );
    }
    tray.icon(Image::from_bytes(include_bytes!("../icons/64x64.png"))?)
        .tooltip("Envelope")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => crate::show_main(app),
            "compose" => {
                crate::show_main(app);
                let _ = app.emit_to("main", "mailto", "mailto:");
            }
            "sync" => {
                if let Some(state) = app.try_state::<AppState>() {
                    state.engine.sync_now(None);
                }
            }
            "quit" => crate::windows::quit(app),
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
        1 => strings::get("native.tray.unreadOne", "Envelope — 1 unread message"),
        n => strings::fill(
            "native.tray.unreadMany",
            "Envelope — {count} unread messages",
            &[("count", &n.to_string())],
        ),
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
