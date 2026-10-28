//! IPC commands called by the UI.

use envelope_core::model::*;
use envelope_core::oauth::{ClientConfig, Provider};
use envelope_core::{AccountPatch, UndoToken};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;

use crate::AppState;

/// Error returned to the UI.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CmdError {
    pub message: String,
    pub kind: &'static str,
}

impl From<envelope_core::Error> for CmdError {
    fn from(e: envelope_core::Error) -> Self {
        use envelope_core::Error as E;
        let kind = match &e {
            E::Auth(_) => "auth",
            E::NotFound(_) => "notFound",
            E::Invalid(_) => "invalid",
            E::Network(_) => "network",
            E::Imap(i) if i.is_auth() => "auth",
            E::Imap(i) if i.is_transient() => "network",
            _ => "error",
        };
        CmdError {
            message: user_message(&e),
            kind,
        }
    }
}

/// The error as shown to people: sentences written for them as they are,
/// short technical details with a readable label.
fn user_message(e: &envelope_core::Error) -> String {
    use envelope_core::Error as E;
    match e {
        E::NotFound(m) | E::Network(m) if m.ends_with('.') && m.contains(' ') => m.clone(),
        E::NotFound(m) => format!("Not found: {m}"),
        E::Network(m) => format!("Network error: {m}"),
        other => other.to_string(),
    }
}

impl From<String> for CmdError {
    fn from(message: String) -> Self {
        CmdError {
            message,
            kind: "error",
        }
    }
}

type R<T> = Result<T, CmdError>;

#[tauri::command]
pub fn app_ready(app: AppHandle, state: State<'_, AppState>) {
    tracing::debug!("interface ready");
    // Launched at login: stay in the tray.
    if state
        .start_hidden
        .swap(false, std::sync::atomic::Ordering::SeqCst)
    {
        return;
    }
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

#[tauri::command]
pub fn autostart_get() -> bool {
    crate::autostart::is_enabled()
}

#[tauri::command]
pub async fn autostart_set(enabled: bool) -> R<()> {
    crate::autostart::set(enabled).await.map_err(CmdError::from)
}

#[tauri::command]
pub fn app_info(state: State<'_, AppState>) -> serde_json::Value {
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "trayAvailable": crate::tray::available(),
        "dataDir": state.engine.paths.data,
        "googleConfigured": !state.engine.oauth_client(Provider::Google).map(|c| c.client_id).unwrap_or_default().is_empty(),
        "microsoftConfigured": !state.engine.oauth_client(Provider::Microsoft).map(|c| c.client_id).unwrap_or_default().is_empty(),
    })
}

// ----- accounts ---------------------------------------------------------------

#[tauri::command]
pub fn accounts(state: State<'_, AppState>) -> R<Vec<Account>> {
    Ok(state.engine.accounts()?)
}

#[tauri::command]
pub async fn discover(email: String) -> R<Discovery> {
    Ok(envelope_core::providers::discover(&email).await)
}

#[tauri::command]
pub async fn account_add_password(state: State<'_, AppState>, input: NewAccount) -> R<Account> {
    Ok(state.engine.add_password_account(input).await?)
}

/// Adds an on-premises Exchange account (Autodiscover when `ews_url` is
/// empty).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn account_add_exchange(
    state: State<'_, AppState>,
    email: String,
    name: String,
    password: String,
    ews_url: Option<String>,
    username: Option<String>,
    accept_invalid_certs: Option<bool>,
    auth: Option<ExchangeAuth>,
) -> R<Account> {
    let input = NewExchangeAccount {
        email,
        name,
        password,
        ews_url,
        username,
        accept_invalid_certs: accept_invalid_certs.unwrap_or(false),
        auth: auth.unwrap_or_default(),
    };
    Ok(state.engine.add_exchange_account(input).await?)
}

#[tauri::command]
pub async fn account_add_oauth(
    app: AppHandle,
    state: State<'_, AppState>,
    provider: Provider,
    login_hint: Option<String>,
) -> R<Account> {
    let opener = app.clone();
    let account = state
        .engine
        .add_oauth_account(provider, login_hint, move |url| {
            opener
                .opener()
                .open_url(url, None::<&str>)
                .map_err(|e| envelope_core::Error::Other(e.to_string()))
        })
        .await?;
    // Bring Envelope back to the front after the browser round trip.
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_focus();
    }
    Ok(account)
}

#[tauri::command]
pub async fn account_update(
    state: State<'_, AppState>,
    id: i64,
    patch: AccountPatch,
) -> R<Account> {
    Ok(state.engine.update_account(id, patch).await?)
}

#[tauri::command]
pub async fn account_remove(state: State<'_, AppState>, id: i64) -> R<()> {
    Ok(state.engine.remove_account(id).await?)
}

#[tauri::command]
pub fn accounts_reorder(state: State<'_, AppState>, ids: Vec<i64>) -> R<()> {
    Ok(state.engine.reorder_accounts(&ids)?)
}

#[tauri::command]
pub fn oauth_client_get(state: State<'_, AppState>, provider: Provider) -> R<ClientConfig> {
    Ok(state.engine.oauth_client(provider)?)
}

#[tauri::command]
pub fn oauth_client_set(
    state: State<'_, AppState>,
    provider: Provider,
    client: ClientConfig,
) -> R<()> {
    Ok(state.engine.set_oauth_client(provider, client)?)
}

#[tauri::command]
pub fn sync_now(state: State<'_, AppState>, account_id: Option<i64>) {
    state.engine.sync_now(account_id);
}

// ----- mail ---------------------------------------------------------------------

#[tauri::command]
pub fn folders(state: State<'_, AppState>) -> R<Vec<Folder>> {
    Ok(state.engine.folders()?)
}

#[tauri::command]
pub fn threads(state: State<'_, AppState>, query: ThreadQuery) -> R<ThreadPage> {
    Ok(state.engine.threads(&query)?)
}

#[tauri::command]
pub fn thread(state: State<'_, AppState>, id: i64) -> R<ThreadDetail> {
    Ok(state.engine.thread(id)?)
}

#[tauri::command]
pub async fn load_bodies(state: State<'_, AppState>, ids: Vec<i64>) -> R<Vec<MessageView>> {
    Ok(state.engine.load_bodies(ids).await?)
}

#[tauri::command]
pub fn set_flags(
    state: State<'_, AppState>,
    thread_ids: Vec<i64>,
    message_ids: Vec<i64>,
    change: FlagChange,
) -> R<()> {
    Ok(state.engine.set_flags(&thread_ids, &message_ids, &change)?)
}

#[tauri::command]
pub fn mark_folder_read(state: State<'_, AppState>, folder_id: i64) -> R<()> {
    Ok(state.engine.mark_folder_read(folder_id)?)
}

#[tauri::command]
pub fn move_threads(
    state: State<'_, AppState>,
    thread_ids: Vec<i64>,
    source: Option<i64>,
    target: MoveTarget,
) -> R<UndoToken> {
    Ok(state.engine.move_threads(&thread_ids, source, &target)?)
}

#[tauri::command]
pub fn undo(state: State<'_, AppState>, token: UndoToken) -> R<()> {
    Ok(state.engine.undo(&token)?)
}

#[tauri::command]
pub fn delete_forever(state: State<'_, AppState>, thread_ids: Vec<i64>) -> R<()> {
    Ok(state.engine.delete_forever(&thread_ids)?)
}

#[tauri::command]
pub fn snooze(state: State<'_, AppState>, thread_ids: Vec<i64>, until: Option<i64>) -> R<()> {
    Ok(state.engine.snooze(&thread_ids, until)?)
}

#[tauri::command]
pub fn set_label(
    state: State<'_, AppState>,
    thread_ids: Vec<i64>,
    folder_id: i64,
    add: bool,
) -> R<()> {
    Ok(state.engine.set_label(&thread_ids, folder_id, add)?)
}

#[tauri::command]
pub async fn message_source(state: State<'_, AppState>, id: i64) -> R<String> {
    Ok(state.engine.message_source(id).await?)
}

#[tauri::command]
pub async fn unsubscribe(app: AppHandle, state: State<'_, AppState>, message_id: i64) -> R<bool> {
    match state.engine.unsubscribe(message_id).await? {
        Some(url) => {
            app.opener()
                .open_url(url, None::<&str>)
                .map_err(|e| CmdError::from(e.to_string()))?;
            Ok(false)
        }
        None => Ok(true),
    }
}

// ----- compose ------------------------------------------------------------------

#[tauri::command]
pub fn drafts(state: State<'_, AppState>) -> R<Vec<Draft>> {
    Ok(state.engine.drafts()?)
}

#[tauri::command]
pub fn draft(state: State<'_, AppState>, id: i64) -> R<Draft> {
    Ok(state.engine.draft(id)?)
}

#[tauri::command]
pub fn save_draft(
    state: State<'_, AppState>,
    id: Option<i64>,
    data: DraftInput,
    upload: bool,
) -> R<i64> {
    Ok(state.engine.save_draft(id, &data, upload)?)
}

#[tauri::command]
pub fn delete_draft(state: State<'_, AppState>, id: i64) -> R<()> {
    Ok(state.engine.delete_draft(id)?)
}

#[tauri::command]
pub fn compose_reply(state: State<'_, AppState>, message_id: i64, mode: String) -> R<DraftInput> {
    Ok(state.engine.compose_reply(message_id, &mode)?)
}

#[tauri::command]
pub async fn send(
    state: State<'_, AppState>,
    draft_id: Option<i64>,
    data: DraftInput,
    delay_ms: i64,
    send_at: Option<i64>,
) -> R<i64> {
    Ok(state
        .engine
        .send(draft_id, &data, delay_ms, send_at)
        .await?)
}

#[tauri::command]
pub fn cancel_send(state: State<'_, AppState>, outbox_id: i64) -> R<Option<i64>> {
    Ok(state.engine.cancel_send(outbox_id)?)
}

#[tauri::command]
pub fn retry_send(state: State<'_, AppState>, outbox_id: i64) -> R<()> {
    Ok(state.engine.retry_send(outbox_id)?)
}

#[tauri::command]
pub fn outbox(state: State<'_, AppState>) -> R<Vec<OutboxItem>> {
    Ok(state.engine.outbox()?)
}

#[tauri::command]
pub fn suggest_contacts(state: State<'_, AppState>, query: String) -> R<Vec<ContactSuggestion>> {
    Ok(state.engine.suggest_contacts(&query, 8)?)
}

#[tauri::command]
pub fn identities(state: State<'_, AppState>) -> R<Vec<Identity>> {
    Ok(state.engine.identities()?)
}

#[tauri::command]
pub fn identity_save(state: State<'_, AppState>, identity: Identity) -> R<i64> {
    Ok(state.engine.save_identity(&identity)?)
}

#[tauri::command]
pub fn identity_delete(state: State<'_, AppState>, id: i64) -> R<()> {
    Ok(state.engine.delete_identity(id)?)
}

/// Metadata about a local file the user picked to attach.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub path: String,
    pub filename: String,
    pub mime_type: String,
    pub size: u64,
}

#[tauri::command]
pub fn file_info(paths: Vec<String>) -> R<Vec<FileInfo>> {
    let mut out = Vec::new();
    for p in paths {
        let meta = std::fs::metadata(&p).map_err(|e| CmdError::from(format!("{p}: {e}")))?;
        if !meta.is_file() {
            continue;
        }
        let path = std::path::Path::new(&p);
        out.push(FileInfo {
            filename: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "file".into()),
            mime_type: mime_for(path),
            size: meta.len(),
            path: p,
        });
    }
    Ok(out)
}

fn mime_for(path: &std::path::Path) -> String {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        "ics" => "text/calendar",
        "zip" => "application/zip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "json" => "application/json",
        "eml" => "message/rfc822",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Writes clipboard/drag image data to a temporary file so it can be attached.
#[tauri::command]
pub fn stash_file(state: State<'_, AppState>, filename: String, data: Vec<u8>) -> R<FileInfo> {
    let dir = state.engine.paths.cache.join("compose").join(uuid_like());
    std::fs::create_dir_all(&dir).map_err(|e| CmdError::from(e.to_string()))?;
    let safe: String = filename
        .chars()
        .map(|c| if c == '/' || c == '\\' { '_' } else { c })
        .collect();
    let path = dir.join(if safe.is_empty() {
        "pasted".to_string()
    } else {
        safe
    });
    std::fs::write(&path, &data).map_err(|e| CmdError::from(e.to_string()))?;
    Ok(FileInfo {
        filename: path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        mime_type: mime_for(&path),
        size: data.len() as u64,
        path: path.to_string_lossy().to_string(),
    })
}

fn uuid_like() -> String {
    format!(
        "{:x}{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
        std::process::id()
    )
}

// ----- attachments -----------------------------------------------------------------

#[tauri::command]
pub async fn attachment_open(app: AppHandle, state: State<'_, AppState>, id: i64) -> R<()> {
    let path = state.engine.attachment_file(id).await?;
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(|e| CmdError::from(e.to_string()))
}

#[tauri::command]
pub async fn attachment_save(state: State<'_, AppState>, id: i64, target: String) -> R<()> {
    let path = state.engine.attachment_file(id).await?;
    tokio::fs::copy(&path, &target)
        .await
        .map_err(|e| CmdError::from(format!("could not save: {e}")))?;
    Ok(())
}

#[tauri::command]
pub async fn attachment_reveal(app: AppHandle, state: State<'_, AppState>, id: i64) -> R<()> {
    let path = state.engine.attachment_file(id).await?;
    app.opener()
        .reveal_item_in_dir(path)
        .map_err(|e| CmdError::from(e.to_string()))
}

// ----- free/busy -------------------------------------------------------------------

#[tauri::command]
pub async fn free_busy(
    state: State<'_, AppState>,
    calendar_id: i64,
    emails: Vec<String>,
    from: i64,
    to: i64,
) -> R<Vec<envelope_core::free_busy::Availability>> {
    Ok(state
        .engine
        .free_busy(calendar_id, emails, from, to)
        .await?)
}

// ----- automatic replies -----------------------------------------------------------

#[tauri::command]
pub async fn auto_reply_get(
    state: State<'_, AppState>,
    account_id: i64,
) -> R<envelope_core::auto_reply::AutoReplyInfo> {
    Ok(state.engine.auto_reply(account_id).await?)
}

#[tauri::command]
pub async fn auto_reply_set(
    state: State<'_, AppState>,
    account_id: i64,
    reply: envelope_core::auto_reply::AutoReply,
) -> R<()> {
    Ok(state.engine.set_auto_reply(account_id, reply).await?)
}

// ----- opened files ----------------------------------------------------------------

#[tauri::command]
pub async fn file_open(
    state: State<'_, AppState>,
    path: String,
) -> R<envelope_core::files::OpenedFile> {
    Ok(state.engine.open_file(std::path::Path::new(&path)).await?)
}

#[tauri::command]
pub async fn file_import_events(
    state: State<'_, AppState>,
    path: String,
    calendar_id: i64,
    uids: Option<Vec<String>>,
) -> R<envelope_core::files::ImportResult> {
    Ok(state
        .engine
        .import_calendar_file(std::path::Path::new(&path), calendar_id, uids)
        .await?)
}

#[tauri::command]
pub async fn file_part_open(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    part_id: String,
) -> R<()> {
    let file = state
        .engine
        .file_part(std::path::Path::new(&path), &part_id)
        .await?;
    app.opener()
        .open_path(file.to_string_lossy(), None::<&str>)
        .map_err(|e| CmdError::from(e.to_string()))
}

#[tauri::command]
pub async fn file_part_save(
    state: State<'_, AppState>,
    path: String,
    part_id: String,
    target: String,
) -> R<()> {
    let file = state
        .engine
        .file_part(std::path::Path::new(&path), &part_id)
        .await?;
    tokio::fs::copy(&file, &target)
        .await
        .map_err(|e| CmdError::from(format!("could not save: {e}")))?;
    Ok(())
}

// ----- settings -------------------------------------------------------------------

#[tauri::command]
pub fn settings_get(state: State<'_, AppState>) -> R<serde_json::Map<String, serde_json::Value>> {
    Ok(state.engine.settings()?)
}

#[tauri::command]
pub fn settings_set(state: State<'_, AppState>, key: String, value: serde_json::Value) -> R<()> {
    Ok(state.engine.set_setting(&key, &value)?)
}

#[tauri::command]
pub fn set_decorations(app: AppHandle, native: bool) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_decorations(native);
    }
}

#[tauri::command]
pub fn set_unread_badge(app: AppHandle, count: i64) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_badge_count(if count > 0 { Some(count) } else { None });
    }
    crate::tray::set_unread(&app, count);
}

// ----- AI -------------------------------------------------------------------------

use envelope_core::ai::{AiConfigView, AiEvent, AiModel, AiPresetView, AiSettings, AiTask};
use tauri::ipc::Channel;

#[tauri::command]
pub async fn ai_config(state: State<'_, AppState>) -> R<AiConfigView> {
    Ok(state.engine.ai_config().await?)
}

#[tauri::command]
pub async fn ai_set_config(
    state: State<'_, AppState>,
    settings: AiSettings,
    api_key: Option<String>,
) -> R<()> {
    Ok(state.engine.set_ai_config(settings, api_key).await?)
}

#[tauri::command]
pub fn ai_presets() -> Vec<AiPresetView> {
    envelope_core::Engine::ai_presets()
}

#[tauri::command]
pub async fn ai_models(
    state: State<'_, AppState>,
    settings: Option<AiSettings>,
    api_key: Option<String>,
) -> R<Vec<AiModel>> {
    Ok(state.engine.ai_models(settings, api_key).await?)
}

#[tauri::command]
pub async fn ai_test(
    state: State<'_, AppState>,
    settings: AiSettings,
    api_key: Option<String>,
) -> R<String> {
    Ok(state.engine.ai_test(settings, api_key).await?)
}

#[tauri::command]
pub async fn ai_run(
    state: State<'_, AppState>,
    run_id: String,
    task: AiTask,
    on_event: Channel<AiEvent>,
) -> R<String> {
    let engine = state.engine.clone();
    let handle = tauri::async_runtime::spawn(async move {
        engine
            .ai_run(task, move |e| {
                let _ = on_event.send(e);
            })
            .await
    });
    state
        .ai_runs
        .lock()
        .insert(run_id.clone(), handle.inner().abort_handle());
    let result = handle.await;
    state.ai_runs.lock().remove(&run_id);
    match result {
        Ok(r) => Ok(r?),
        Err(_) => Err(CmdError {
            message: "Cancelled".into(),
            kind: "cancelled",
        }),
    }
}

#[tauri::command]
pub fn ai_cancel(state: State<'_, AppState>, run_id: String) {
    if let Some(h) = state.ai_runs.lock().remove(&run_id) {
        h.abort();
    }
}

// ----- calendar ----------------------------------------------------------------------

use envelope_core::calendar::{
    CalendarView, EditScope, EventDetail, EventInput, InviteInfo, Occurrence,
};
use envelope_core::envelope_ical::{EventTime, PartStat};

#[tauri::command]
pub fn calendars(state: State<'_, AppState>) -> R<Vec<CalendarView>> {
    Ok(state.engine.calendars()?)
}

#[tauri::command]
pub fn calendar_set_visible(state: State<'_, AppState>, id: i64, visible: bool) -> R<()> {
    Ok(state.engine.set_calendar_visible(id, visible)?)
}

#[tauri::command]
pub fn calendar_set_color(state: State<'_, AppState>, id: i64, color: String) -> R<()> {
    Ok(state.engine.set_calendar_color(id, &color)?)
}

#[tauri::command]
pub fn occurrences(state: State<'_, AppState>, from: i64, to: i64) -> R<Vec<Occurrence>> {
    Ok(state.engine.occurrences(from, to)?)
}

#[tauri::command]
pub fn event_detail(
    state: State<'_, AppState>,
    event_id: i64,
    recurrence_id: Option<EventTime>,
) -> R<EventDetail> {
    Ok(state.engine.event_detail(event_id, recurrence_id)?)
}

#[tauri::command]
pub async fn event_create(state: State<'_, AppState>, input: EventInput) -> R<i64> {
    Ok(state.engine.create_event(input).await?)
}

#[tauri::command]
pub async fn event_update(
    state: State<'_, AppState>,
    event_id: i64,
    recurrence_id: Option<EventTime>,
    scope: EditScope,
    input: EventInput,
) -> R<()> {
    Ok(state
        .engine
        .update_event(event_id, recurrence_id, scope, input)
        .await?)
}

#[tauri::command]
pub async fn event_move(
    state: State<'_, AppState>,
    event_id: i64,
    recurrence_id: Option<EventTime>,
    scope: EditScope,
    start: i64,
    end: i64,
) -> R<()> {
    Ok(state
        .engine
        .move_event(event_id, recurrence_id, scope, start, end)
        .await?)
}

#[tauri::command]
pub async fn event_delete(
    state: State<'_, AppState>,
    event_id: i64,
    recurrence_id: Option<EventTime>,
    scope: EditScope,
) -> R<()> {
    Ok(state
        .engine
        .delete_event(event_id, recurrence_id, scope)
        .await?)
}

#[tauri::command]
pub async fn event_respond(
    state: State<'_, AppState>,
    event_id: i64,
    status: PartStat,
    comment: Option<String>,
) -> R<()> {
    Ok(state
        .engine
        .respond_event(event_id, status, comment)
        .await?)
}

#[tauri::command]
pub async fn invite_info(state: State<'_, AppState>, message_id: i64) -> R<InviteInfo> {
    Ok(state.engine.invite_info(message_id).await?)
}

#[tauri::command]
pub async fn invite_respond(
    state: State<'_, AppState>,
    message_id: i64,
    status: PartStat,
    comment: Option<String>,
) -> R<()> {
    Ok(state
        .engine
        .respond_invite(message_id, status, comment)
        .await?)
}

// ----- contacts ----------------------------------------------------------------------

use envelope_core::contacts::{ContactBook, ContactInput, ContactPage, ContactView};

#[tauri::command]
pub async fn contact_books(state: State<'_, AppState>) -> R<Vec<ContactBook>> {
    let engine = state.engine.clone();
    tauri::async_runtime::spawn_blocking(move || engine.contact_books())
        .await
        .map_err(|e| CmdError::from(e.to_string()))?
        .map_err(Into::into)
}

#[tauri::command]
pub async fn contacts_list(
    state: State<'_, AppState>,
    query: String,
    book_id: Option<i64>,
    offset: i64,
    limit: i64,
) -> R<ContactPage> {
    Ok(state
        .engine
        .contacts_list(&query, book_id, offset, limit)
        .await?)
}

#[tauri::command]
pub async fn contact_get(state: State<'_, AppState>, id: i64) -> R<ContactView> {
    Ok(state.engine.contact(id).await?)
}

#[tauri::command]
pub async fn contact_save(state: State<'_, AppState>, contact: ContactInput) -> R<i64> {
    Ok(state.engine.save_contact(contact).await?)
}

#[tauri::command]
pub async fn contact_delete(state: State<'_, AppState>, id: i64) -> R<()> {
    Ok(state.engine.delete_contact(id).await?)
}
