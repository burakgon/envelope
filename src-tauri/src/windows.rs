//! Windows of their own: a conversation, or a message being written, beside
//! the main window. They load the same interface (`index.html?window=…`), use
//! the same commands and hear the same engine events, so what is done in one
//! shows in the others; links, files and notifications still go to the main
//! window. Each kind of window opens at the size the last one had.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use serde::Deserialize;
use tauri::{AppHandle, LogicalSize, Manager, PhysicalSize, WebviewUrl, WebviewWindowBuilder};

use crate::AppState;

/// What a window of its own shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    /// A conversation (labels `message-<thread id>`).
    Thread,
    /// A message being written (labels `compose-<draft id>`).
    Compose,
}

impl Kind {
    /// How its labels begin (the capabilities name windows by them).
    fn prefix(self) -> &'static str {
        match self {
            Kind::Thread => "message-",
            Kind::Compose => "compose-",
        }
    }

    /// Its name in the interface's address (`?window=thread`).
    fn route(self) -> &'static str {
        match self {
            Kind::Thread => "thread",
            Kind::Compose => "compose",
        }
    }

    /// The size of the first window of the kind.
    fn first_size(self) -> (f64, f64) {
        match self {
            Kind::Thread => (920.0, 840.0),
            Kind::Compose => (760.0, 700.0),
        }
    }

    /// The setting that keeps the size of the last window of the kind.
    fn size_setting(self) -> String {
        format!("windowSize.{}", self.route())
    }

    fn of(label: &str) -> Option<Kind> {
        [Kind::Thread, Kind::Compose]
            .into_iter()
            .find(|k| label.starts_with(k.prefix()))
    }
}

/// Sizes of the windows open now, by label, until they close.
static SIZES: Mutex<Option<HashMap<String, (f64, f64)>>> = Mutex::new(None);
/// Numbers the windows of new messages, which have no draft yet.
static NEW: AtomicU32 = AtomicU32::new(1);

fn label(kind: Kind, id: Option<i64>) -> String {
    match id {
        Some(id) => format!("{}{id}", kind.prefix()),
        None => format!(
            "{}new-{}",
            kind.prefix(),
            NEW.fetch_add(1, Ordering::Relaxed)
        ),
    }
}

fn setting(app: &AppHandle, key: &str) -> Option<serde_json::Value> {
    app.try_state::<AppState>()
        .and_then(|s| s.engine.get_setting(key).ok().flatten())
}

/// Brings a window of its own forward when one is open for this conversation
/// or draft.
pub fn focus(app: &AppHandle, kind: Kind, id: i64) -> bool {
    let Some(w) = app.get_webview_window(&label(kind, Some(id))) else {
        return false;
    };
    let _ = w.unminimize();
    let _ = w.show();
    let _ = w.set_focus();
    true
}

/// Opens a conversation or a draft (`None`: a new message) in a window of its
/// own, or brings forward the one already open. It shows once its interface
/// has loaded (`app_ready`).
pub fn open(app: &AppHandle, kind: Kind, id: Option<i64>, title: &str) -> tauri::Result<()> {
    if let Some(id) = id
        && focus(app, kind, id)
    {
        return Ok(());
    }
    let (width, height) = setting(app, &kind.size_setting())
        .and_then(|v| Some((v.get("width")?.as_f64()?, v.get("height")?.as_f64()?)))
        .filter(|(w, h)| *w >= 200.0 && *h >= 200.0)
        .unwrap_or(kind.first_size());
    let mut url = format!("index.html?window={}", kind.route());
    if let Some(id) = id {
        url.push_str(&format!("&id={id}"));
    }
    let native = setting(app, "nativeTitlebar")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let window = WebviewWindowBuilder::new(app, label(kind, id), WebviewUrl::App(url.into()))
        .title(title)
        .inner_size(width, height)
        .min_inner_size(480.0, 360.0)
        .decorations(native)
        .visible(false)
        // Files dropped on a message being written are attached by the page.
        .disable_drag_drop_handler()
        .build()?;
    #[cfg(target_os = "linux")]
    crate::enable_spell_checking(&window);
    Ok(())
}

/// Whether the main window is on screen, where what another window hands
/// over to it (a toast with Undo) can be seen.
pub fn main_shown(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .is_some_and(|w| w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false))
}

/// Notes the size of a window of its own as it changes (not while maximized).
pub fn resized(window: &tauri::Window, size: PhysicalSize<u32>) {
    if Kind::of(window.label()).is_none() || window.is_maximized().unwrap_or(false) {
        return;
    }
    let scale = window.scale_factor().unwrap_or(1.0);
    let size: LogicalSize<f64> = size.to_logical(scale);
    SIZES
        .lock()
        .get_or_insert_with(HashMap::new)
        .insert(window.label().to_string(), (size.width, size.height));
}

/// Remembers the size of a window of its own that closed, for the next one of
/// its kind.
pub fn closed(window: &tauri::Window) {
    let Some(kind) = Kind::of(window.label()) else {
        return;
    };
    let size = SIZES.lock().as_mut().and_then(|s| s.remove(window.label()));
    if let (Some((width, height)), Some(state)) =
        (size, window.app_handle().try_state::<AppState>())
    {
        let value = serde_json::json!({ "width": width.round(), "height": height.round() });
        if let Err(e) = state.engine.set_setting(&kind.size_setting(), &value) {
            tracing::warn!("could not remember the size of a window: {e}");
        }
    }
}

/// The windows of their own that are open.
fn others(app: &AppHandle) -> Vec<tauri::WebviewWindow> {
    app.webview_windows()
        .into_values()
        .filter(|w| Kind::of(w.label()).is_some())
        .collect()
}

/// Closes the windows of their own as the main window closes: each saves what
/// is being written in it first.
pub fn close_all(app: &AppHandle) {
    for w in others(app) {
        let _ = w.close();
    }
}

/// Quits Envelope once the windows of their own have closed (saving what is
/// being written in them), or after a few seconds.
pub fn quit(app: &AppHandle) {
    close_all(app);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        for _ in 0..30 {
            if others(&app).is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        app.exit(0);
    });
}
