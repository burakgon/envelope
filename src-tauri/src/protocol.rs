//! `att://` — serves attachments and inline (`cid:`) images to the web UI.
//!
//! - `att://localhost/inline/<message id>/<content id>`
//! - `att://localhost/file/<attachment id>`

use tauri::http::{Request, Response, StatusCode};
use tauri::{Manager, Runtime, UriSchemeContext, UriSchemeResponder};

use crate::AppState;

fn not_found() -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header("Access-Control-Allow-Origin", "*")
        .body(Vec::new())
        .unwrap_or_default()
}

pub fn handle<R: Runtime>(ctx: UriSchemeContext<'_, R>, request: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let app = ctx.app_handle().clone();
    let path = request.uri().path().to_string();
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<AppState>() else {
            responder.respond(not_found());
            return;
        };
        let engine = state.engine.clone();
        let parts: Vec<String> = path
            .trim_start_matches('/')
            .split('/')
            .map(|p| percent_encoding::percent_decode_str(p).decode_utf8_lossy().to_string())
            .collect();
        let result = match parts.as_slice() {
            [kind, message, cid] if kind == "inline" => match message.parse::<i64>() {
                Ok(id) => engine.inline_image(id, cid).await.ok(),
                Err(_) => None,
            },
            [kind, id] if kind == "file" => match id.parse::<i64>() {
                Ok(id) => match engine.attachment_file(id).await {
                    Ok(path) => {
                        let mime = mime_of(&path);
                        Some((mime, path))
                    }
                    Err(_) => None,
                },
                Err(_) => None,
            },
            _ => None,
        };
        let response = match result {
            Some((mime, path)) => match tokio::fs::read(&path).await {
                Ok(data) => Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", mime)
                    .header("Cache-Control", "private, max-age=31536000, immutable")
                    .header("Access-Control-Allow-Origin", "*")
                    .body(data)
                    .unwrap_or_else(|_| not_found()),
                Err(_) => not_found(),
            },
            None => not_found(),
        };
        responder.respond(response);
    });
}

fn mime_of(path: &std::path::Path) -> String {
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
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        "txt" => "text/plain; charset=utf-8",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        _ => "application/octet-stream",
    }
    .to_string()
}
