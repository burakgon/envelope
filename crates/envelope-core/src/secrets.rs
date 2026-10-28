//! Secret storage: the desktop keyring (Secret Service / KWallet via D-Bus, or
//! the portal-backed file keyring inside Flatpak), with an owner-only file as a
//! last resort on systems without any keyring.

use std::collections::HashMap;
use std::path::PathBuf;

use parking_lot::Mutex;
use tokio::sync::OnceCell;

use crate::error::{Error, Result};

const APP_ATTR: &str = "envelope";

pub struct Secrets {
    keyring: OnceCell<Option<oo7::Keyring>>,
    fallback: PathBuf,
    cache: Mutex<HashMap<String, String>>,
    force_file: bool,
}

impl Secrets {
    pub fn new(fallback: PathBuf) -> Secrets {
        Secrets {
            keyring: OnceCell::new(),
            fallback,
            cache: Mutex::new(HashMap::new()),
            force_file: std::env::var("ENVELOPE_SECRETS").is_ok_and(|v| v == "file"),
        }
    }

    async fn keyring(&self) -> Option<&oo7::Keyring> {
        if self.force_file {
            return None;
        }
        self.keyring
            .get_or_init(|| async {
                match tokio::time::timeout(std::time::Duration::from_secs(10), oo7::Keyring::new())
                    .await
                {
                    Ok(Ok(k)) => {
                        if let Err(e) = k.unlock().await {
                            tracing::warn!("keyring unlock failed: {e}");
                        }
                        Some(k)
                    }
                    Ok(Err(e)) => {
                        tracing::warn!("system keyring unavailable, using file storage: {e}");
                        None
                    }
                    Err(_) => {
                        tracing::warn!("system keyring timed out, using file storage");
                        None
                    }
                }
            })
            .await
            .as_ref()
    }

    /// Whether secrets end up in the plain fallback file.
    pub async fn is_insecure(&self) -> bool {
        self.keyring().await.is_none()
    }

    pub async fn get(&self, key: &str) -> Result<Option<String>> {
        if let Some(v) = self.cache.lock().get(key) {
            return Ok(Some(v.clone()));
        }
        let value = match self.keyring().await {
            Some(k) => {
                let items = k
                    .search_items(&[("application", APP_ATTR), ("key", key)])
                    .await
                    .map_err(|e| Error::Other(format!("keyring: {e}")))?;
                match items.first() {
                    Some(item) => {
                        let secret = item
                            .secret()
                            .await
                            .map_err(|e| Error::Other(format!("keyring: {e}")))?;
                        Some(String::from_utf8_lossy(secret.as_bytes()).into_owned())
                    }
                    None => self.file_read()?.remove(key),
                }
            }
            None => self.file_read()?.remove(key),
        };
        if let Some(v) = &value {
            self.cache.lock().insert(key.to_string(), v.clone());
        }
        Ok(value)
    }

    /// Puts a secret into the in-memory cache only, keeping tests away from
    /// the desktop keyring.
    #[cfg(test)]
    pub(crate) fn preload(&self, key: &str, value: &str) {
        self.cache.lock().insert(key.to_string(), value.to_string());
    }

    pub async fn set(&self, key: &str, value: &str) -> Result<()> {
        match self.keyring().await {
            Some(k) => {
                k.create_item(
                    &format!("Envelope: {key}"),
                    &[("application", APP_ATTR), ("key", key)],
                    value,
                    true,
                )
                .await
                .map_err(|e| Error::Other(format!("keyring: {e}")))?;
            }
            None => {
                let mut all = self.file_read()?;
                all.insert(key.to_string(), value.to_string());
                self.file_write(&all)?;
            }
        }
        self.cache.lock().insert(key.to_string(), value.to_string());
        Ok(())
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        self.cache.lock().remove(key);
        if let Some(k) = self.keyring().await {
            let _ = k.delete(&[("application", APP_ATTR), ("key", key)]).await;
        }
        let mut all = self.file_read()?;
        if all.remove(key).is_some() {
            self.file_write(&all)?;
        }
        Ok(())
    }

    /// Deletes every secret whose key starts with `prefix`.
    pub async fn delete_prefix(&self, prefix: &str) -> Result<()> {
        let keys: Vec<String> = self
            .cache
            .lock()
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        for k in keys {
            self.delete(&k).await?;
        }
        if let Some(k) = self.keyring().await
            && let Ok(items) = k.search_items(&[("application", APP_ATTR)]).await
        {
            for item in items {
                if let Ok(attrs) = item.attributes().await
                    && attrs.get("key").is_some_and(|key| key.starts_with(prefix))
                {
                    let _ = item.delete().await;
                }
            }
        }
        let mut all = self.file_read()?;
        let before = all.len();
        all.retain(|k, _| !k.starts_with(prefix));
        if all.len() != before {
            self.file_write(&all)?;
        }
        Ok(())
    }

    fn file_read(&self) -> Result<HashMap<String, String>> {
        match std::fs::read(&self.fallback) {
            Ok(data) => Ok(serde_json::from_slice(&data).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(e.into()),
        }
    }

    fn file_write(&self, all: &HashMap<String, String>) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(dir) = self.fallback.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.fallback.with_extension("tmp");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(&serde_json::to_vec(all)?)?;
        f.sync_all()?;
        std::fs::rename(tmp, &self.fallback)?;
        Ok(())
    }
}

pub fn password_key(account_id: i64) -> String {
    format!("account:{account_id}:password")
}

pub fn oauth_key(account_id: i64) -> String {
    format!("account:{account_id}:oauth")
}

pub fn account_prefix(account_id: i64) -> String {
    format!("account:{account_id}:")
}
