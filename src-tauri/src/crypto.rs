//! IPC commands for OpenPGP and S/MIME: keys and certificates, settings,
//! composing and opening messages.

use envelope_core::crypto::{
    CertificateInfo, ComposeCrypto, CryptoSettings, KeyInfo, OpenedMessage, OwnKey,
};
use envelope_core::model::Address;
use tauri::State;

use crate::AppState;
use crate::commands::CmdError;

type R<T> = Result<T, CmdError>;

/// Key files larger than this are not keys.
const MAX_KEY_FILE: u64 = 10 * 1024 * 1024;

#[tauri::command]
pub fn crypto_settings_get(state: State<'_, AppState>) -> R<CryptoSettings> {
    Ok(state.engine.crypto_settings()?)
}

#[tauri::command]
pub fn crypto_settings_set(state: State<'_, AppState>, settings: CryptoSettings) -> R<()> {
    Ok(state.engine.set_crypto_settings(&settings)?)
}

#[tauri::command]
pub fn crypto_own_keys(state: State<'_, AppState>) -> R<Vec<OwnKey>> {
    Ok(state.engine.own_keys()?)
}

#[tauri::command]
pub async fn crypto_generate_key(
    state: State<'_, AppState>,
    email: String,
    name: String,
    passphrase: Option<String>,
) -> R<KeyInfo> {
    Ok(state
        .engine
        .generate_key(&email, &name, passphrase.as_deref())
        .await?)
}

#[tauri::command]
pub async fn crypto_import_text(
    state: State<'_, AppState>,
    text: String,
    passphrase: Option<String>,
    own_email: Option<String>,
) -> R<Vec<KeyInfo>> {
    Ok(state
        .engine
        .import_keys(text.as_bytes(), passphrase.as_deref(), own_email.as_deref())
        .await?)
}

#[tauri::command]
pub async fn crypto_import_file(
    state: State<'_, AppState>,
    path: String,
    passphrase: Option<String>,
    own_email: Option<String>,
) -> R<Vec<KeyInfo>> {
    let size = tokio::fs::metadata(&path)
        .await
        .map_err(|e| CmdError::from(format!("{path}: {e}")))?
        .len();
    if size > MAX_KEY_FILE {
        return Err(CmdError::from(
            "This file is too large to be a key.".to_string(),
        ));
    }
    let data = tokio::fs::read(&path)
        .await
        .map_err(|e| CmdError::from(format!("{path}: {e}")))?;
    Ok(state
        .engine
        .import_keys(&data, passphrase.as_deref(), own_email.as_deref())
        .await?)
}

#[tauri::command]
pub fn crypto_export_public(state: State<'_, AppState>, fingerprint: String) -> R<String> {
    Ok(state.engine.export_public_key(&fingerprint)?)
}

#[tauri::command]
pub async fn crypto_export_secret(
    state: State<'_, AppState>,
    fingerprint: String,
    passphrase: Option<String>,
) -> R<String> {
    Ok(state
        .engine
        .export_secret_key(&fingerprint, passphrase.as_deref())
        .await?)
}

/// Saves a key to a file (secret keys readable by the user alone).
#[tauri::command]
pub async fn crypto_export_file(
    state: State<'_, AppState>,
    fingerprint: String,
    secret: bool,
    passphrase: Option<String>,
    path: String,
) -> R<()> {
    let armored = if secret {
        state
            .engine
            .export_secret_key(&fingerprint, passphrase.as_deref())
            .await?
    } else {
        state.engine.export_public_key(&fingerprint)?
    };
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if secret {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    options
        .open(&path)
        .and_then(|mut f| f.write_all(armored.as_bytes()))
        .map_err(|e| CmdError::from(format!("could not save: {e}")))
}

#[tauri::command]
pub async fn crypto_delete_key(state: State<'_, AppState>, fingerprint: String) -> R<()> {
    Ok(state.engine.delete_key(&fingerprint).await?)
}

#[tauri::command]
pub fn crypto_keys(state: State<'_, AppState>, query: String) -> R<Vec<KeyInfo>> {
    Ok(state.engine.keys(&query)?)
}

#[tauri::command]
pub fn crypto_set_verified(
    state: State<'_, AppState>,
    fingerprint: String,
    verified: bool,
) -> R<()> {
    Ok(state.engine.set_key_verified(&fingerprint, verified)?)
}

#[tauri::command]
pub fn crypto_set_own_key(state: State<'_, AppState>, email: String, fingerprint: String) -> R<()> {
    Ok(state.engine.set_own_key(&email, &fingerprint)?)
}

#[tauri::command]
pub async fn crypto_unlock(
    state: State<'_, AppState>,
    fingerprint: String,
    passphrase: String,
) -> R<()> {
    Ok(state.engine.unlock_key(&fingerprint, &passphrase).await?)
}

#[tauri::command]
pub fn crypto_lock(state: State<'_, AppState>) {
    state.engine.crypto_lock();
}

#[tauri::command]
pub fn crypto_compose_status(
    state: State<'_, AppState>,
    account_id: i64,
    identity_id: Option<i64>,
    from: Option<Address>,
    recipients: Vec<String>,
    reply_to: Option<i64>,
) -> R<ComposeCrypto> {
    Ok(state
        .engine
        .compose_crypto(account_id, identity_id, from, &recipients, reply_to)?)
}

#[tauri::command]
pub async fn crypto_lookup(
    state: State<'_, AppState>,
    emails: Vec<String>,
    keyserver: bool,
    explicit: bool,
) -> R<Vec<KeyInfo>> {
    Ok(state
        .engine
        .lookup_keys(&emails, keyserver, explicit)
        .await?)
}

#[tauri::command]
pub async fn crypto_open(state: State<'_, AppState>, message_id: i64) -> R<OpenedMessage> {
    Ok(state.engine.open_message(message_id).await?)
}

// ----- S/MIME ------------------------------------------------------------------

#[tauri::command]
pub fn smime_certificates(state: State<'_, AppState>, query: String) -> R<Vec<CertificateInfo>> {
    Ok(state.engine.smime_certificates(&query)?)
}

/// Imports a certificate file: a `.p12` / `.pfx` with its password (the
/// user's own), or other people's certificates.
#[tauri::command]
pub async fn smime_import_file(
    state: State<'_, AppState>,
    path: String,
    password: Option<String>,
    own_email: Option<String>,
) -> R<Vec<CertificateInfo>> {
    let size = tokio::fs::metadata(&path)
        .await
        .map_err(|e| CmdError::from(format!("{path}: {e}")))?
        .len();
    if size > MAX_KEY_FILE {
        return Err(CmdError::from(
            "This file is too large to be a certificate.".to_string(),
        ));
    }
    let data = tokio::fs::read(&path)
        .await
        .map_err(|e| CmdError::from(format!("{path}: {e}")))?;
    Ok(state
        .engine
        .import_smime(&data, password.as_deref(), own_email.as_deref())
        .await?)
}

/// A certificate as PEM text.
#[tauri::command]
pub fn smime_export(state: State<'_, AppState>, fingerprint: String) -> R<String> {
    Ok(state.engine.export_smime_certificate(&fingerprint)?)
}

/// Saves a certificate: DER for `.cer` and `.der` files (as Windows expects
/// them), PEM otherwise.
#[tauri::command]
pub async fn smime_export_file(
    state: State<'_, AppState>,
    fingerprint: String,
    path: String,
) -> R<()> {
    let lower = path.to_lowercase();
    let data = if lower.ends_with(".cer") || lower.ends_with(".der") {
        state.engine.smime_certificate_der(&fingerprint)?
    } else {
        state
            .engine
            .export_smime_certificate(&fingerprint)?
            .into_bytes()
    };
    tokio::fs::write(&path, data)
        .await
        .map_err(|e| CmdError::from(format!("could not save: {e}")))
}

#[tauri::command]
pub async fn smime_delete(state: State<'_, AppState>, fingerprint: String) -> R<()> {
    Ok(state.engine.delete_smime_certificate(&fingerprint).await?)
}
