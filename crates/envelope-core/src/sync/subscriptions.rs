//! Calendars subscribed to by URL (`webcal:` and `https:` iCalendar
//! feeds): read-only, fetched again every few hours, and only stored again
//! when the feed changed.

use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::calendar::CalendarView;
use crate::db::calendar::{self as cdb, CalendarRow};
use crate::db::now_ms;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::EngineEvent;

/// How often a feed is fetched again.
const REFRESH: i64 = 6 * 3600 * 1000;
/// Feeds larger than this are refused.
const MAX_FEED: usize = 20 * 1024 * 1024;

const COLORS: &[&str] = &[
    "#0ea5e9", "#14b8a6", "#f59e0b", "#ec4899", "#8b5cf6", "#22c55e",
];

/// `webcal://` is `https://` for every server that still offers it.
fn feed_url(input: &str) -> Result<url::Url> {
    let trimmed = input.trim();
    let text = if let Some(rest) = trimmed
        .strip_prefix("webcals://")
        .or_else(|| trimmed.strip_prefix("webcal://"))
    {
        format!("https://{rest}")
    } else if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    let url = url::Url::parse(&text)
        .map_err(|_| Error::Invalid("This is not a calendar address.".into()))?;
    if !matches!(url.scheme(), "https" | "http") || url.host_str().is_none() {
        return Err(Error::Invalid("This is not a calendar address.".into()));
    }
    Ok(url)
}

fn http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(15))
        .user_agent(concat!("Envelope/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| Error::Other(e.to_string()))
}

/// A fetched feed, or `None` when it did not change since `state`.
struct Feed {
    text: String,
    etag: Option<String>,
    modified: Option<String>,
}

async fn fetch(
    client: &reqwest::Client,
    url: &str,
    state: &cdb::CalendarSyncState,
) -> Result<Option<Feed>> {
    let mut request = client.get(url).header("Accept", "text/calendar, */*;q=0.5");
    if let Some(etag) = &state.ctag {
        request = request.header("If-None-Match", etag);
    }
    if let Some(modified) = &state.sync_token {
        request = request.header("If-Modified-Since", modified);
    }
    let response = request
        .send()
        .await
        .map_err(|e| Error::Network(e.to_string()))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(Error::Invalid(
            "This calendar is private; its address needs a password.".into(),
        ));
    }
    if !status.is_success() {
        return Err(Error::Network(format!(
            "the calendar server answered {status}"
        )));
    }
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let (etag, modified) = (header("etag"), header("last-modified"));
    let bytes = response
        .bytes()
        .await
        .map_err(|e| Error::Network(e.to_string()))?;
    if bytes.len() > MAX_FEED {
        return Err(Error::Invalid(
            "This calendar is too large to subscribe to.".into(),
        ));
    }
    Ok(Some(Feed {
        text: String::from_utf8_lossy(&bytes).into_owned(),
        etag,
        modified,
    }))
}

fn digest(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The feed's own name, if it has one.
fn feed_name(data: &envelope_ical::CalendarData) -> Option<String> {
    data.components
        .iter()
        .find(|c| c.is("VCALENDAR"))
        .and_then(|c| c.value("X-WR-CALNAME"))
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty())
}

/// Fetches a subscribed calendar and stores its events when they changed.
async fn refresh_one(engine: &Engine, client: &reqwest::Client, cal: &CalendarRow) -> Result<bool> {
    let mut state = cal.sync_state.clone();
    let Some(feed) = fetch(client, &cal.remote_id, &state).await? else {
        state.last_sync = Some(now_ms());
        engine
            .db
            .write(|tx| cdb::set_calendar_sync_state(tx, cal.id, &state))?;
        return Ok(false);
    };
    let hash = digest(&feed.text);
    let changed = state.cursor.as_deref() != Some(hash.as_str());
    if changed {
        let data = envelope_ical::parse_calendar(&feed.text)
            .map_err(|_| Error::Invalid("This address does not lead to a calendar.".into()))?;
        let events: Vec<_> = data
            .events
            .iter()
            .cloned()
            .map(|mut e| {
                crate::files::utc_where_unknown(&mut e, &data.tz);
                // A feed's own alarms would remind of things nobody asked for.
                e.alarms.clear();
                e
            })
            .collect();
        engine.replace_events(cal.id, &events)?;
    }
    state.ctag = feed.etag;
    state.sync_token = feed.modified;
    state.cursor = Some(hash);
    state.last_sync = Some(now_ms());
    engine
        .db
        .write(|tx| cdb::set_calendar_sync_state(tx, cal.id, &state))?;
    Ok(changed)
}

/// Keeps subscribed calendars up to date in the background.
pub(crate) fn spawn(engine: Engine) {
    tokio::spawn(async move {
        let Ok(client) = http() else {
            return;
        };
        loop {
            let due: Vec<CalendarRow> = engine
                .db
                .read(cdb::list_calendars)
                .unwrap_or_default()
                .into_iter()
                .filter(|c| c.kind == "subscription")
                .filter(|c| {
                    c.sync_state
                        .last_sync
                        .is_none_or(|t| now_ms() - t >= REFRESH)
                })
                .collect();
            for cal in due {
                match refresh_one(&engine, &client, &cal).await {
                    Ok(true) => engine.emit(EngineEvent::CalendarChanged {
                        calendar_ids: vec![cal.id],
                    }),
                    Ok(false) => {}
                    Err(e) => {
                        tracing::info!(calendar = cal.id, "subscribed calendar not refreshed: {e}")
                    }
                }
            }
            tokio::select! {
                _ = engine.shutdown.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(15 * 60)) => {}
            }
        }
    });
}

impl Engine {
    /// Subscribes to the calendar at `url` and returns it, with its events
    /// already fetched.
    pub async fn subscribe_calendar(&self, url: &str) -> Result<CalendarView> {
        let url = feed_url(url)?;
        let address = url.to_string();
        if self
            .db
            .read(cdb::list_calendars)?
            .iter()
            .any(|c| c.kind == "subscription" && c.remote_id == address)
        {
            return Err(Error::Invalid(
                "You are already subscribed to this calendar.".into(),
            ));
        }
        let client = http()?;
        let feed = fetch(&client, &address, &Default::default())
            .await?
            .ok_or_else(|| Error::Network("the calendar server sent nothing".into()))?;
        let data = envelope_ical::parse_calendar(&feed.text)
            .map_err(|_| Error::Invalid("This address does not lead to a calendar.".into()))?;
        let name =
            feed_name(&data).unwrap_or_else(|| url.host_str().unwrap_or("Calendar").to_owned());
        let count = self.db.read(cdb::calendar_count)?;
        let color = COLORS[count as usize % COLORS.len()];
        let id = self
            .db
            .write(|tx| cdb::create_subscription(tx, &address, &name, color))?;
        let cal = self.db.read(|c| cdb::get_calendar(c, id))?;
        // Store what was just fetched instead of asking again.
        let events: Vec<_> = data
            .events
            .iter()
            .cloned()
            .map(|mut e| {
                crate::files::utc_where_unknown(&mut e, &data.tz);
                // A feed's own alarms would remind of things nobody asked for.
                e.alarms.clear();
                e
            })
            .collect();
        self.replace_events(id, &events)?;
        let state = cdb::CalendarSyncState {
            ctag: feed.etag,
            sync_token: feed.modified,
            cursor: Some(digest(&feed.text)),
            last_sync: Some(now_ms()),
        };
        self.db
            .write(|tx| cdb::set_calendar_sync_state(tx, id, &state))?;
        self.emit(EngineEvent::CalendarChanged {
            calendar_ids: vec![id],
        });
        self.calendars()?
            .into_iter()
            .find(|c| c.id == cal.id)
            .ok_or_else(|| Error::NotFound("calendar".into()))
    }

    /// Fetches a subscribed calendar now.
    pub async fn refresh_subscription(&self, calendar_id: i64) -> Result<()> {
        let cal = self.db.read(|c| cdb::get_calendar(c, calendar_id))?;
        if cal.kind != "subscription" {
            return Err(Error::Invalid(
                "This calendar is not a subscription.".into(),
            ));
        }
        if refresh_one(self, &http()?, &cal).await? {
            self.emit(EngineEvent::CalendarChanged {
                calendar_ids: vec![calendar_id],
            });
        }
        Ok(())
    }

    /// Stops a subscription and removes its events.
    pub fn unsubscribe_calendar(&self, calendar_id: i64) -> Result<()> {
        let cal = self.db.read(|c| cdb::get_calendar(c, calendar_id))?;
        if cal.kind != "subscription" {
            return Err(Error::Invalid(
                "Only subscribed calendars can be removed here.".into(),
            ));
        }
        self.db.write(|tx| cdb::delete_calendar(tx, calendar_id))?;
        self.emit(EngineEvent::CalendarChanged {
            calendar_ids: vec![calendar_id],
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn understands_calendar_addresses() {
        assert_eq!(
            feed_url("webcal://example.com/cal.ics").unwrap().as_str(),
            "https://example.com/cal.ics"
        );
        assert_eq!(
            feed_url(" webcals://example.com/a ").unwrap().as_str(),
            "https://example.com/a"
        );
        assert_eq!(
            feed_url("calendar.example.org/h.ics").unwrap().as_str(),
            "https://calendar.example.org/h.ics"
        );
        assert_eq!(
            feed_url("http://example.com/x.ics").unwrap().scheme(),
            "http"
        );
        assert!(feed_url("ftp://example.com/x.ics").is_err());
        assert!(feed_url("file:///etc/passwd").is_err());
        assert!(feed_url("").is_err());
    }
}
