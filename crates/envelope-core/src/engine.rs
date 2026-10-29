//! The engine: owns storage, account workers and background schedulers, and
//! exposes the operations the UI calls.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::db::mail::{self, AccountRow};
use crate::error::{Error, Result};
use crate::model::*;
use crate::oauth::TokenManager;
use crate::secrets::{self, Secrets};

/// Receives engine events (implemented by the UI shell).
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: EngineEvent);
}

/// Where Envelope keeps its files.
#[derive(Debug, Clone)]
pub struct Paths {
    /// Database and persistent data.
    pub data: PathBuf,
    /// Downloaded attachments and other disposable files.
    pub cache: PathBuf,
}

impl Paths {
    /// Standard XDG locations (`~/.local/share/envelope`, `~/.cache/envelope`).
    pub fn standard() -> Paths {
        let data = std::env::var_os("ENVELOPE_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::data_dir().map(|d| d.join("envelope")))
            .unwrap_or_else(|| PathBuf::from(".envelope"));
        let cache = std::env::var_os("ENVELOPE_CACHE_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::cache_dir().map(|d| d.join("envelope")))
            .unwrap_or_else(|| data.join("cache"));
        Paths { data, cache }
    }
}

pub(crate) struct Worker {
    pub wake: Arc<Notify>,
    pub stop: CancellationToken,
}

pub type ImapSlot = Arc<tokio::sync::Mutex<Option<envelope_imap::Client>>>;

pub struct Inner {
    pub db: Db,
    pub secrets: Arc<Secrets>,
    pub tokens: TokenManager,
    pub paths: Paths,
    events: Box<dyn EventSink>,
    pub(crate) workers: Mutex<HashMap<i64, Worker>>,
    sync_states: Mutex<HashMap<i64, SyncState>>,
    /// Connections used for user-initiated fetches (bodies, attachments).
    pub(crate) interactive: Mutex<HashMap<i64, ImapSlot>>,
    pub(crate) outbox_wake: Notify,
    pub(crate) calendar_wake: Notify,
    /// Wakes the contacts sync (local edits, account changes, manual sync).
    pub(crate) contacts_wake: Notify,
    /// Wakes the contact photo worker.
    pub(crate) photos_wake: Notify,
    /// Wakes the task sync (local edits, account changes, manual sync).
    pub(crate) tasks_wake: Notify,
    /// OpenPGP session state: unlocked passphrases, decrypted messages.
    pub(crate) crypto: crate::crypto::Session,
    pub(crate) shutdown: CancellationToken,
}

/// Cheaply cloneable engine handle.
#[derive(Clone)]
pub struct Engine {
    pub(crate) inner: Arc<Inner>,
}

impl std::ops::Deref for Engine {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.inner
    }
}

impl Engine {
    /// Opens storage and starts background work for all enabled accounts.
    pub fn start(paths: Paths, events: impl EventSink) -> Result<Engine> {
        std::fs::create_dir_all(&paths.data)?;
        std::fs::create_dir_all(&paths.cache)?;
        // Attachments of encrypted messages opened in an earlier session.
        let _ = std::fs::remove_dir_all(paths.cache.join("decrypted"));
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let db = Db::open(&paths.data.join("envelope.db"))?;
        let secrets = Arc::new(Secrets::new(paths.data.join("secrets.json")));
        let engine = Engine {
            inner: Arc::new(Inner {
                db,
                tokens: TokenManager::new(secrets.clone()),
                secrets,
                paths,
                events: Box::new(events),
                workers: Mutex::new(HashMap::new()),
                sync_states: Mutex::new(HashMap::new()),
                interactive: Mutex::new(HashMap::new()),
                outbox_wake: Notify::new(),
                calendar_wake: Notify::new(),
                contacts_wake: Notify::new(),
                photos_wake: Notify::new(),
                tasks_wake: Notify::new(),
                crypto: crate::crypto::Session::default(),
                shutdown: CancellationToken::new(),
            }),
        };
        for account in engine.db.read(mail::list_accounts)? {
            if account.enabled {
                engine.start_worker(account.id);
            }
        }
        crate::sync::outbox::spawn_scheduler(engine.clone());
        crate::sync::calendar::spawn_scheduler(engine.clone());
        crate::sync::contacts::spawn_scheduler(engine.clone());
        crate::sync::tasks::spawn_scheduler(engine.clone());
        crate::sync::spawn_housekeeping(engine.clone());
        Ok(engine)
    }

    /// Stops all background work.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
        for (_, w) in self.workers.lock().drain() {
            w.stop.cancel();
        }
    }

    pub fn emit(&self, event: EngineEvent) {
        self.events.emit(event);
    }

    pub(crate) fn set_sync_state(&self, account_id: i64, state: SyncState) {
        let changed = {
            let mut states = self.sync_states.lock();
            let prev = states.insert(account_id, state.clone());
            prev.as_ref() != Some(&state)
        };
        if changed {
            self.emit(EngineEvent::SyncStatus {
                account_id,
                sync: state,
            });
        }
    }

    pub fn sync_state(&self, account_id: i64) -> SyncState {
        self.sync_states
            .lock()
            .get(&account_id)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn start_worker(&self, account_id: i64) {
        let mut workers = self.workers.lock();
        if workers.contains_key(&account_id) {
            return;
        }
        let worker = Worker {
            wake: Arc::new(Notify::new()),
            stop: self.shutdown.child_token(),
        };
        let engine = self.clone();
        let wake = worker.wake.clone();
        let stop = worker.stop.clone();
        workers.insert(account_id, worker);
        tokio::spawn(async move {
            crate::sync::run_account(engine, account_id, wake, stop).await;
        });
    }

    pub(crate) fn stop_worker(&self, account_id: i64) {
        if let Some(w) = self.workers.lock().remove(&account_id) {
            w.stop.cancel();
        }
        self.interactive.lock().remove(&account_id);
    }

    /// Wakes an account worker (new local changes or a manual refresh).
    pub fn wake(&self, account_id: i64) {
        if let Some(w) = self.workers.lock().get(&account_id) {
            w.wake.notify_one();
        }
    }

    pub fn sync_now(&self, account_id: Option<i64>) {
        self.calendar_wake.notify_one();
        self.contacts_wake.notify_one();
        self.tasks_wake.notify_one();
        match account_id {
            Some(id) => self.wake(id),
            None => {
                for w in self.workers.lock().values() {
                    w.wake.notify_one();
                }
            }
        }
    }

    // ----- accounts --------------------------------------------------------

    fn account_view(&self, row: AccountRow) -> Account {
        let has_calendar = match row.kind {
            AccountKind::Gmail | AccountKind::Microsoft | AccountKind::Exchange => true,
            AccountKind::Imap => row.config.caldav_url.is_some(),
        };
        let has_contacts = match row.kind {
            AccountKind::Gmail | AccountKind::Microsoft | AccountKind::Exchange => true,
            AccountKind::Imap => row.config.carddav_url.is_some(),
        };
        Account {
            sync: self.sync_state(row.id),
            id: row.id,
            kind: row.kind,
            provider: row.provider,
            email: row.email,
            name: row.name,
            label: row.label,
            color: row.color,
            sort_order: row.sort_order,
            enabled: row.enabled,
            config: row.config,
            has_calendar,
            has_contacts,
        }
    }

    pub fn accounts(&self) -> Result<Vec<Account>> {
        let rows = self.db.read(mail::list_accounts)?;
        Ok(rows.into_iter().map(|r| self.account_view(r)).collect())
    }

    pub fn account(&self, id: i64) -> Result<Account> {
        let row = self.db.read(|c| mail::get_account(c, id))?;
        Ok(self.account_view(row))
    }

    pub(crate) fn account_row(&self, id: i64) -> Result<AccountRow> {
        self.db.read(|c| mail::get_account(c, id))
    }

    pub(crate) fn next_color(&self) -> Result<String> {
        const PALETTE: [&str; 8] = [
            "#6d5efc", "#0ea5e9", "#10b981", "#f59e0b", "#ef4444", "#ec4899", "#8b5cf6", "#14b8a6",
        ];
        let n = self.db.read(|c| {
            Ok(c.query_row("SELECT count(*) FROM accounts", [], |r| r.get::<_, i64>(0))?)
        })?;
        Ok(PALETTE[(n as usize) % PALETTE.len()].to_string())
    }

    /// Verifies credentials and adds an IMAP/SMTP account.
    pub async fn add_password_account(&self, input: NewAccount) -> Result<Account> {
        let email = input.email.trim().to_string();
        if !email.contains('@') {
            return Err(Error::Invalid("Enter a valid e-mail address".into()));
        }
        let mut config = input.config.clone();
        config.auth = AuthMethod::Password;
        let imap = config
            .imap
            .clone()
            .ok_or_else(|| Error::Invalid("Incoming (IMAP) server is required".into()))?;
        let provider = if input.provider.is_empty() {
            "generic".to_string()
        } else {
            input.provider.clone()
        };

        // Verify IMAP login. Some providers (iCloud) want just the local part.
        let mut user = if imap.username.is_empty() {
            email.clone()
        } else {
            imap.username.clone()
        };
        let attempt = crate::sync::imap::connect_with(
            &imap,
            &config,
            &envelope_imap::Auth::Password {
                user: user.clone(),
                password: input.password.clone(),
            },
        )
        .await;
        match attempt {
            Ok(mut c) => {
                let _ = c.logout().await;
            }
            Err(e) if e.is_auth() && provider == "icloud" && user.contains('@') => {
                let local = email.split('@').next().unwrap_or_default().to_string();
                let mut c = crate::sync::imap::connect_with(
                    &imap,
                    &config,
                    &envelope_imap::Auth::Password {
                        user: local.clone(),
                        password: input.password.clone(),
                    },
                )
                .await?;
                let _ = c.logout().await;
                user = local;
            }
            Err(e) => {
                return Err(explain_setup_error(
                    "incoming (IMAP)",
                    &imap.host,
                    imap.port,
                    e,
                ));
            }
        }
        if let Some(imap_cfg) = config.imap.as_mut() {
            imap_cfg.username = if user == email { String::new() } else { user };
        }
        if let Some(smtp) = &config.smtp {
            let smtp_user = if smtp.username.is_empty() {
                email.clone()
            } else {
                smtp.username.clone()
            };
            crate::smtp::test(
                smtp,
                &crate::smtp::SmtpAuth::Password {
                    user: smtp_user,
                    password: input.password.clone(),
                },
                config.accept_invalid_certs,
            )
            .await
            .map_err(|e| explain_setup_error("outgoing (SMTP)", &smtp.host, smtp.port, e))?;
        }

        let color = self.next_color()?;
        let name = if input.name.trim().is_empty() {
            email.split('@').next().unwrap_or(&email).to_string()
        } else {
            input.name.trim().to_string()
        };
        let id = self.db.write(|tx| {
            mail::insert_account(
                tx,
                AccountKind::Imap,
                &provider,
                &email,
                &name,
                &color,
                &config,
            )
        })?;
        self.secrets
            .set(&secrets::password_key(id), &input.password)
            .await?;
        self.start_worker(id);
        self.calendar_wake.notify_one();
        self.contacts_wake.notify_one();
        self.tasks_wake.notify_one();
        self.emit(EngineEvent::AccountsChanged);
        self.account(id)
    }

    /// Runs the browser sign-in for Google or Microsoft and adds the account.
    pub async fn add_oauth_account(
        &self,
        provider: crate::oauth::Provider,
        login_hint: Option<String>,
        open: impl FnOnce(String) -> Result<()>,
    ) -> Result<Account> {
        let client = self.oauth_client(provider)?;
        let auth = crate::oauth::authorize(provider, client, login_hint.as_deref(), open).await?;
        let email = auth.email.to_lowercase();
        // Re-authorizing an existing account just refreshes its tokens.
        if let Some(existing) = self
            .db
            .read(mail::list_accounts)?
            .into_iter()
            .find(|a| a.email.eq_ignore_ascii_case(&email))
        {
            self.tokens.store(existing.id, &auth.tokens).await?;
            self.stop_worker(existing.id);
            self.start_worker(existing.id);
            return self.account(existing.id);
        }
        let (kind, preset_id, auth_method) = match provider {
            crate::oauth::Provider::Google => {
                (AccountKind::Gmail, "gmail", AuthMethod::OAuthGoogle)
            }
            crate::oauth::Provider::Microsoft => (
                AccountKind::Microsoft,
                "outlook",
                AuthMethod::OAuthMicrosoft,
            ),
        };
        let preset = crate::providers::preset(preset_id);
        let server = |s: Option<(&'static str, u16, Security)>| {
            s.map(|(host, port, security)| ServerConfig {
                host: host.into(),
                port,
                security,
                username: String::new(),
            })
        };
        let config = AccountConfig {
            auth: auth_method,
            imap: preset.and_then(|p| server(p.imap)),
            smtp: preset.and_then(|p| server(p.smtp)),
            ..Default::default()
        };
        let color = self.next_color()?;
        let name = auth
            .name
            .clone()
            .unwrap_or_else(|| email.split('@').next().unwrap_or(&email).to_string());
        let id = self.db.write(|tx| {
            mail::insert_account(tx, kind, preset_id, &email, &name, &color, &config)
        })?;
        self.tokens.store(id, &auth.tokens).await?;
        self.start_worker(id);
        self.calendar_wake.notify_one();
        self.contacts_wake.notify_one();
        self.tasks_wake.notify_one();
        self.emit(EngineEvent::AccountsChanged);
        self.account(id)
    }

    /// OAuth client registration (settings override → build defaults).
    pub fn oauth_client(
        &self,
        provider: crate::oauth::Provider,
    ) -> Result<crate::oauth::ClientConfig> {
        let key = match provider {
            crate::oauth::Provider::Google => "oauth.google",
            crate::oauth::Provider::Microsoft => "oauth.microsoft",
        };
        let custom: Option<crate::oauth::ClientConfig> =
            self.db.read(|c| crate::db::misc::kv_get(c, key))?;
        Ok(custom
            .filter(|c| !c.client_id.trim().is_empty())
            .unwrap_or_else(|| provider.default_client()))
    }

    pub fn set_oauth_client(
        &self,
        provider: crate::oauth::Provider,
        client: crate::oauth::ClientConfig,
    ) -> Result<()> {
        let key = match provider {
            crate::oauth::Provider::Google => "oauth.google",
            crate::oauth::Provider::Microsoft => "oauth.microsoft",
        };
        self.db
            .write(|tx| crate::db::misc::kv_set(tx, key, &client))
    }

    /// Updates editable account fields.
    pub async fn update_account(&self, id: i64, patch: AccountPatch) -> Result<Account> {
        let current = self.account_row(id)?;
        let restart = patch.config.is_some() || patch.password.is_some() || patch.enabled.is_some();
        self.db.write(|tx| {
            if let Some(name) = &patch.name {
                tx.execute(
                    "UPDATE accounts SET name = ? WHERE id = ?",
                    rusqlite::params![name, id],
                )?;
            }
            if let Some(label) = &patch.label {
                let label = Some(label.trim()).filter(|l| !l.is_empty());
                tx.execute(
                    "UPDATE accounts SET label = ? WHERE id = ?",
                    rusqlite::params![label, id],
                )?;
            }
            if let Some(color) = &patch.color {
                tx.execute(
                    "UPDATE accounts SET color = ? WHERE id = ?",
                    rusqlite::params![color, id],
                )?;
            }
            if let Some(enabled) = patch.enabled {
                tx.execute(
                    "UPDATE accounts SET enabled = ? WHERE id = ?",
                    rusqlite::params![enabled as i64, id],
                )?;
            }
            if let Some(config) = &patch.config {
                tx.execute(
                    "UPDATE accounts SET config = ? WHERE id = ?",
                    rusqlite::params![serde_json::to_string(config)?, id],
                )?;
            }
            Ok(())
        })?;
        if let Some(pw) = &patch.password {
            self.secrets.set(&secrets::password_key(id), pw).await?;
        }
        if restart {
            self.stop_worker(id);
            if patch.enabled.unwrap_or(current.enabled) {
                self.start_worker(id);
            }
            self.calendar_wake.notify_one();
            self.contacts_wake.notify_one();
            self.tasks_wake.notify_one();
            // The account's lists show or hide with it.
            self.emit(EngineEvent::TasksChanged {
                list_ids: Vec::new(),
            });
        }
        self.emit(EngineEvent::AccountsChanged);
        self.account(id)
    }

    /// Removes an account and all of its local data.
    pub async fn remove_account(&self, id: i64) -> Result<()> {
        self.stop_worker(id);
        self.db.write_async(move |tx| {
            tx.execute("DELETE FROM message_fts WHERE rowid IN (SELECT id FROM messages WHERE account_id = ?)", [id])?;
            tx.execute("DELETE FROM accounts WHERE id = ?", [id])?;
            Ok(())
        })
        .await?;
        self.secrets
            .delete_prefix(&secrets::account_prefix(id))
            .await?;
        let _ = std::fs::remove_dir_all(self.paths.cache.join(format!("account-{id}")));
        self.emit(EngineEvent::AccountsChanged);
        self.emit(EngineEvent::TasksChanged {
            list_ids: Vec::new(),
        });
        Ok(())
    }

    pub fn reorder_accounts(&self, ids: &[i64]) -> Result<()> {
        self.db.write(|tx| {
            for (i, id) in ids.iter().enumerate() {
                tx.execute(
                    "UPDATE accounts SET sort_order = ? WHERE id = ?",
                    rusqlite::params![i as i64, id],
                )?;
            }
            Ok(())
        })?;
        self.emit(EngineEvent::AccountsChanged);
        Ok(())
    }

    // ----- settings ----------------------------------------------------------

    pub fn get_setting(&self, key: &str) -> Result<Option<serde_json::Value>> {
        self.db
            .read(|c| crate::db::misc::kv_get(c, &format!("ui.{key}")))
    }

    pub fn set_setting(&self, key: &str, value: &serde_json::Value) -> Result<()> {
        self.db
            .write(|tx| crate::db::misc::kv_set(tx, &format!("ui.{key}"), value))
    }

    /// All UI settings as one JSON object.
    pub fn settings(&self) -> Result<serde_json::Map<String, serde_json::Value>> {
        self.db.read(|c| {
            let mut st = c.prepare("SELECT key, value FROM kv WHERE key LIKE 'ui.%'")?;
            let rows =
                st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut map = serde_json::Map::new();
            for row in rows {
                let (k, v) = row?;
                if let Ok(v) = serde_json::from_str(&v) {
                    map.insert(k.trim_start_matches("ui.").to_string(), v);
                }
            }
            Ok(map)
        })
    }
}

/// Editable account fields.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AccountPatch {
    pub name: Option<String>,
    pub label: Option<String>,
    pub color: Option<String>,
    pub enabled: Option<bool>,
    pub config: Option<AccountConfig>,
    pub password: Option<String>,
}

/// Turns a connection or login failure while adding an account into advice
/// that says which server failed and what to check.
fn explain_setup_error(server: &str, host: &str, port: u16, e: Error) -> Error {
    if e.is_auth() {
        return Error::Auth(format!(
            "The {server} server rejected the e-mail and password. Check them, or use an app password if your provider requires one."
        ));
    }
    let detail = e.to_string();
    let lower = detail.to_lowercase();
    let message = if lower.contains("refused") {
        format!(
            "Nothing answered at {host}:{port}. Check the {server} server name, port and security."
        )
    } else if lower.contains("certificate")
        || lower.contains("unknownissuer")
        || lower.contains("invalid peer")
    {
        format!(
            "The {server} server's certificate isn't trusted. If this is your own server, tick “Accept self-signed certificates”."
        )
    } else if lower.contains("lookup")
        || lower.contains("resolve")
        || lower.contains("name or service")
        || lower.contains("dns")
    {
        format!("Couldn't find the {server} server {host}. Check its name.")
    } else if lower.contains("timed out") || lower.contains("timeout") {
        format!(
            "The {server} server {host}:{port} didn't answer in time. Check the port and security, or try again."
        )
    } else if lower.contains("tls")
        || lower.contains("handshake")
        || lower.contains("wrong version")
    {
        format!(
            "Couldn't start a secure connection with {host}:{port}. The {server} security setting (SSL/TLS or STARTTLS) may not match the port."
        )
    } else {
        return e;
    };
    Error::Invalid(message)
}
