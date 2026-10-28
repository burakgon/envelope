//! AI assistance: provider configuration and mail-aware tasks on top of
//! `envelope-ai`. Nothing is sent anywhere unless the user enabled AI and
//! asked for something.

use envelope_ai::prompts::{self, Budget, MailMessageCtx, RewriteStyle, Task, UserCtx};
use envelope_ai::{AiClient, ChatEvent, ModelInfo, ProviderConfig};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::db::{mail, misc};
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::MessageView;

const SETTINGS_KEY: &str = "ai.settings";

fn key_secret(provider: &str) -> String {
    format!("ai:{provider}:key")
}

/// Persisted AI preferences (the API key lives in the keyring).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiSettings {
    pub enabled: bool,
    /// Preset id (`openrouter`, `openai`, `anthropic`, `gemini`, `ollama`, `custom`, …).
    pub provider: String,
    pub base_url: String,
    pub model: String,
    /// Free-form notes about the user's writing style.
    pub custom_instructions: String,
    /// Preferred output language (None = match the mail).
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConfigView {
    #[serde(flatten)]
    pub settings: AiSettings,
    pub has_key: bool,
    pub local: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiPresetView {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub requires_key: bool,
    pub key_url: Option<String>,
    pub default_model: Option<String>,
    pub local: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiModel {
    pub id: String,
    pub name: String,
    pub context_length: Option<u32>,
    pub input_price: Option<f64>,
    pub output_price: Option<f64>,
}

impl From<ModelInfo> for AiModel {
    fn from(m: ModelInfo) -> Self {
        AiModel {
            id: m.id,
            name: m.name,
            context_length: m.context_length,
            input_price: m.input_price_per_mtok,
            output_price: m.output_price_per_mtok,
        }
    }
}

/// Something the user asked the assistant to do.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AiTask {
    #[serde(rename_all = "camelCase")]
    Summarize { thread_id: i64 },
    #[serde(rename_all = "camelCase")]
    QuickReplies { thread_id: i64 },
    #[serde(rename_all = "camelCase")]
    DraftReply { thread_id: i64, instruction: String },
    Compose {
        instruction: String,
        #[serde(default)]
        recipients: Vec<String>,
    },
    Rewrite {
        text: String,
        style: String,
    },
    Translate {
        text: String,
        language: String,
    },
    Ask {
        question: String,
    },
    SearchQuery {
        question: String,
    },
    #[serde(rename_all = "camelCase")]
    ExtractEvents { message_id: i64 },
}

/// A message used to answer a question.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSource {
    pub index: usize,
    pub message_id: i64,
    pub thread_id: i64,
    pub subject: String,
    pub from: String,
    pub date: i64,
}

/// Progress of a running task.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum AiEvent {
    Delta { text: String },
    Sources { sources: Vec<AiSource> },
    Done { text: String },
}

fn ai_err(e: envelope_ai::AiError) -> Error {
    Error::Other(e.to_string())
}

fn message_ctx(m: &MessageView) -> MailMessageCtx {
    let fmt = |a: &crate::model::Address| a.to_header();
    let body = m
        .body
        .as_ref()
        .and_then(|b| b.text.clone().or_else(|| b.html.as_deref().map(crate::mime::html_to_text)))
        .unwrap_or_else(|| m.snippet.clone());
    MailMessageCtx {
        id: m.id.to_string(),
        from: m.from.as_ref().map(fmt).unwrap_or_default(),
        to: m.to.iter().map(fmt).collect(),
        cc: m.cc.iter().map(fmt).collect(),
        date: chrono::DateTime::from_timestamp_millis(m.date)
            .map(|d| d.with_timezone(&chrono::Local).to_rfc3339())
            .unwrap_or_default(),
        subject: m.subject.clone(),
        body_text: body,
    }
}

const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "to", "in", "on", "at", "for", "from", "with", "about", "is", "are", "was",
    "were", "be", "my", "me", "i", "you", "your", "what", "when", "where", "which", "who", "how", "did", "do", "does",
    "can", "could", "would", "should", "there", "this", "that", "it", "any", "find", "show", "tell", "mail", "email",
    "emails", "message", "messages", "ne", "zaman", "nerede", "hangi", "bir", "ve", "ile", "mi", "mı", "mu", "mü",
    "bu", "şu", "der", "die", "das", "und", "ist", "le", "la", "les", "et", "est", "el", "los", "y", "es",
];

impl Engine {
    pub fn ai_settings(&self) -> Result<AiSettings> {
        Ok(self.db.read(|c| misc::kv_get(c, SETTINGS_KEY))?.unwrap_or_else(|| AiSettings {
            enabled: false,
            provider: "openrouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            model: "openrouter/auto".into(),
            ..Default::default()
        }))
    }

    pub async fn ai_config(&self) -> Result<AiConfigView> {
        let settings = self.ai_settings()?;
        let has_key = self.secrets.get(&key_secret(&settings.provider)).await?.is_some();
        let local = envelope_ai::preset(&settings.provider).is_some_and(|p| p.local);
        Ok(AiConfigView { settings, has_key, local })
    }

    /// Saves settings; `api_key` = Some("") removes the stored key.
    pub async fn set_ai_config(&self, settings: AiSettings, api_key: Option<String>) -> Result<()> {
        if let Some(key) = api_key {
            if key.trim().is_empty() {
                self.secrets.delete(&key_secret(&settings.provider)).await?;
            } else {
                self.secrets.set(&key_secret(&settings.provider), key.trim()).await?;
            }
        }
        self.db.write(|tx| misc::kv_set(tx, SETTINGS_KEY, &settings))
    }

    pub fn ai_presets() -> Vec<AiPresetView> {
        envelope_ai::presets()
            .iter()
            .map(|p| AiPresetView {
                id: p.id.into(),
                name: p.name.into(),
                kind: p.kind.display_name().into(),
                base_url: p.base_url.into(),
                requires_key: p.requires_key,
                key_url: p.key_url.map(str::to_string),
                default_model: p.default_model.map(str::to_string),
                local: p.local,
            })
            .collect()
    }

    async fn ai_client_for(&self, settings: &AiSettings, key: Option<String>) -> Result<AiClient> {
        let key = match key {
            Some(k) if !k.trim().is_empty() => Some(k),
            _ => self.secrets.get(&key_secret(&settings.provider)).await?,
        };
        let mut config = ProviderConfig::from_preset(&settings.provider, key.clone())
            .unwrap_or_else(|| ProviderConfig::new(envelope_ai::ProviderKind::OpenAiCompatible, "", key, ""));
        if !settings.base_url.trim().is_empty() {
            config.base_url = settings.base_url.trim().to_string();
        }
        config.model = settings.model.trim().to_string();
        AiClient::new(config).map_err(ai_err)
    }

    /// Lists models for (possibly unsaved) settings.
    pub async fn ai_models(&self, settings: Option<AiSettings>, key: Option<String>) -> Result<Vec<AiModel>> {
        let settings = match settings {
            Some(s) => s,
            None => self.ai_settings()?,
        };
        let client = self.ai_client_for(&settings, key).await?;
        let mut models: Vec<AiModel> = client.list_models().await.map_err(ai_err)?.into_iter().map(Into::into).collect();
        models.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(models)
    }

    /// Checks that the provider answers. Returns a short report.
    pub async fn ai_test(&self, settings: AiSettings, key: Option<String>) -> Result<String> {
        let client = self.ai_client_for(&settings, key).await?;
        let report = client.test_connection().await.map_err(ai_err)?;
        Ok(match (report.model, report.models_listed) {
            (Some(m), _) => format!("Connected · {m} answered in {} ms", report.latency.as_millis()),
            (None, Some(n)) => format!("Connected · {n} models available"),
            _ => format!("Connected in {} ms", report.latency.as_millis()),
        })
    }

    fn user_ctx(&self, account_id: Option<i64>) -> Result<UserCtx> {
        let settings = self.ai_settings()?;
        let accounts = self.db.read(mail::list_accounts)?;
        let account = account_id
            .and_then(|id| accounts.iter().find(|a| a.id == id))
            .or(accounts.first());
        Ok(UserCtx {
            name: account.map(|a| a.name.clone()).unwrap_or_default(),
            email: account.map(|a| a.email.clone()).unwrap_or_default(),
            timezone: iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into()),
            now: chrono::Local::now().to_rfc3339(),
            language: settings.language.clone().filter(|l| !l.is_empty()),
            writing_style: Some(settings.custom_instructions.clone()).filter(|s| !s.trim().is_empty()),
        })
    }

    async fn thread_ctx(&self, thread_id: i64) -> Result<(i64, Vec<MailMessageCtx>)> {
        let detail = self.thread(thread_id)?;
        let missing: Vec<i64> = detail.messages.iter().filter(|m| m.body.is_none()).map(|m| m.id).collect();
        let messages = if missing.is_empty() {
            detail.messages
        } else {
            let ids: Vec<i64> = detail.messages.iter().map(|m| m.id).collect();
            self.load_bodies(ids).await.unwrap_or(detail.messages)
        };
        Ok((detail.thread.account_id, messages.iter().map(message_ctx).collect()))
    }

    /// Finds messages relevant to a question with the local index.
    fn retrieve(&self, question: &str, limit: usize) -> Result<Vec<MessageView>> {
        let terms: Vec<String> = question
            .split(|c: char| !c.is_alphanumeric())
            .map(|w| w.to_lowercase())
            .filter(|w| w.chars().count() > 2 && !STOPWORDS.contains(&w.as_str()))
            .take(12)
            .collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let fts = terms.iter().map(|t| format!("\"{}\"*", t.replace('"', ""))).collect::<Vec<_>>().join(" OR ");
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT m.id FROM message_fts f JOIN messages m ON m.id = f.rowid
                 WHERE message_fts MATCH ?
                 ORDER BY bm25(message_fts, 4.0, 2.0, 1.0, 1.0, 1.0) - (m.date / 86400000.0 / 3650.0) LIMIT ?",
            )?;
            let ids: Vec<i64> = st
                .query_map(rusqlite::params![fts, limit as i64 * 2], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut out: Vec<MessageView> = Vec::new();
            let mut threads = std::collections::HashSet::new();
            for id in ids {
                let m = mail::get_message(c, id)?;
                // One message per conversation keeps the context diverse.
                if threads.insert(m.thread_id) {
                    out.push(m);
                }
                if out.len() >= limit {
                    break;
                }
            }
            Ok(out)
        })
    }

    /// Runs an AI task, streaming partial text through `emit`.
    pub async fn ai_run(&self, task: AiTask, emit: impl Fn(AiEvent) + Send + Sync) -> Result<String> {
        let settings = self.ai_settings()?;
        if !settings.enabled {
            return Err(Error::Invalid("AI is turned off. Enable it in Settings → AI.".into()));
        }
        let client = self.ai_client_for(&settings, None).await?;
        let budget = Budget::default();

        let (kind, messages) = match &task {
            AiTask::Summarize { thread_id } => {
                let (account, thread) = self.thread_ctx(*thread_id).await?;
                (Task::SummarizeThread, prompts::summarize_thread(&self.user_ctx(Some(account))?, &thread, budget))
            }
            AiTask::QuickReplies { thread_id } => {
                let (account, thread) = self.thread_ctx(*thread_id).await?;
                (Task::QuickReplies, prompts::quick_replies(&self.user_ctx(Some(account))?, &thread, budget))
            }
            AiTask::DraftReply { thread_id, instruction } => {
                let (account, thread) = self.thread_ctx(*thread_id).await?;
                (
                    Task::DraftReply,
                    prompts::draft_reply(&self.user_ctx(Some(account))?, &thread, instruction, budget),
                )
            }
            AiTask::Compose { instruction, recipients } => {
                (Task::Compose, prompts::compose(&self.user_ctx(None)?, instruction, recipients))
            }
            AiTask::Rewrite { text, style } => {
                let style = match style.as_str() {
                    "improve" => RewriteStyle::Improve,
                    "shorter" => RewriteStyle::Shorter,
                    "longer" => RewriteStyle::Longer,
                    "formal" => RewriteStyle::Formal,
                    "friendly" => RewriteStyle::Friendly,
                    "fixGrammar" => RewriteStyle::FixGrammar,
                    "simplify" => RewriteStyle::Simplify,
                    "confident" => RewriteStyle::Confident,
                    other => RewriteStyle::Custom(other.to_string()),
                };
                let kind = if matches!(style, RewriteStyle::FixGrammar) { Task::Proofread } else { Task::Rewrite };
                (kind, prompts::rewrite(&self.user_ctx(None)?, text, &style))
            }
            AiTask::Translate { text, language } => (Task::Translate, prompts::translate(text, language)),
            AiTask::Ask { question } => {
                let mut sources = self.retrieve(question, 8)?;
                // Bodies make answers much better; fetch missing ones (bounded).
                let missing: Vec<i64> = sources.iter().filter(|m| m.body.is_none()).map(|m| m.id).collect();
                if !missing.is_empty()
                    && let Ok(loaded) =
                        tokio::time::timeout(std::time::Duration::from_secs(8), self.load_bodies(missing)).await
                {
                    for m in loaded.unwrap_or_default() {
                        if let Some(s) = sources.iter_mut().find(|s| s.id == m.id) {
                            *s = m;
                        }
                    }
                }
                let ctx: Vec<MailMessageCtx> = sources.iter().map(message_ctx).collect();
                emit(AiEvent::Sources {
                    sources: sources
                        .iter()
                        .enumerate()
                        .map(|(i, m)| AiSource {
                            index: i + 1,
                            message_id: m.id,
                            thread_id: m.thread_id,
                            subject: m.subject.clone(),
                            from: m.from.as_ref().map(|f| f.name.clone().unwrap_or_else(|| f.email.clone())).unwrap_or_default(),
                            date: m.date,
                        })
                        .collect(),
                });
                (Task::AskInbox, prompts::ask_inbox(&self.user_ctx(None)?, question, &ctx, budget))
            }
            AiTask::SearchQuery { question } => (Task::NlToSearch, prompts::nl_to_search(&self.user_ctx(None)?, question)),
            AiTask::ExtractEvents { message_id } => {
                let loaded = self.load_bodies(vec![*message_id]).await?;
                let m = loaded.first().ok_or_else(|| Error::NotFound("message".into()))?;
                (
                    Task::ExtractEvents,
                    prompts::extract_events(&self.user_ctx(Some(m.account_id))?, &message_ctx(m), budget),
                )
            }
        };

        let streaming = !kind.json();
        let mut stream = client.chat_stream(kind.request(messages));
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            match event.map_err(ai_err)? {
                ChatEvent::Delta(d) => {
                    text.push_str(&d);
                    if streaming {
                        emit(AiEvent::Delta { text: d });
                    }
                }
                ChatEvent::Done { .. } => break,
            }
        }

        // Normalize structured answers to JSON the UI understands.
        let out = match &task {
            AiTask::QuickReplies { .. } => serde_json::to_string(&prompts::parse_quick_replies(&text).map_err(ai_err)?)?,
            AiTask::Compose { .. } => serde_json::to_string(&prompts::parse_compose(&text).map_err(ai_err)?)?,
            AiTask::ExtractEvents { .. } => serde_json::to_string(&prompts::parse_events(&text).map_err(ai_err)?)?,
            AiTask::SearchQuery { .. } => prompts::parse_search_query(&text),
            _ => text.trim().to_string(),
        };
        emit(AiEvent::Done { text: out.clone() });
        Ok(out)
    }
}
