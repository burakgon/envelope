//! Free/busy queries (`freebusy.query`).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{time, wire};
use crate::client::{ApiRequest, GoogleClient, Service};
use crate::error::{Error, Result};

/// A free/busy query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreeBusyRequest {
    /// Start of the interval.
    pub time_min: DateTime<Utc>,
    /// End of the interval.
    pub time_max: DateTime<Utc>,
    /// Calendar ids (or addresses of people and resources) to query.
    pub calendar_ids: Vec<String>,
}

/// The result of a free/busy query, keyed by calendar id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FreeBusy {
    /// Busy intervals per queried calendar.
    pub calendars: BTreeMap<String, CalendarBusy>,
}

/// Busy intervals of one calendar.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarBusy {
    /// Busy intervals, sorted as returned by Google.
    pub busy: Vec<BusyPeriod>,
    /// Error reasons for this calendar (e.g. `notFound`), if it could not
    /// be queried.
    pub errors: Vec<String>,
}

/// A busy interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BusyPeriod {
    /// Inclusive start.
    pub start: DateTime<Utc>,
    /// Exclusive end.
    pub end: DateTime<Utc>,
}

impl GoogleClient {
    /// Returns when the given calendars are busy.
    pub async fn free_busy(&self, query: &FreeBusyRequest) -> Result<FreeBusy> {
        if query.time_max <= query.time_min {
            return Err(Error::InvalidInput(
                "free/busy interval must end after it starts".to_owned(),
            ));
        }
        let body = json!({
            "timeMin": time::rfc3339(&query.time_min),
            "timeMax": time::rfc3339(&query.time_max),
            "items": query
                .calendar_ids
                .iter()
                .map(|id| json!({ "id": id }))
                .collect::<Vec<_>>(),
        });
        let request =
            ApiRequest::new(Method::POST, Service::Calendar, &["freeBusy"]).json(&body)?;
        let response: wire::FreeBusyResponse = self.send_json(&request, "freebusy.query").await?;
        Ok(FreeBusy {
            calendars: response
                .calendars
                .into_iter()
                .map(|(id, calendar)| {
                    let busy = calendar
                        .busy
                        .iter()
                        .filter_map(|period| {
                            Some(BusyPeriod {
                                start: time::parse_timestamp(period.start.as_deref()?)?,
                                end: time::parse_timestamp(period.end.as_deref()?)?,
                            })
                        })
                        .collect();
                    let errors = calendar
                        .errors
                        .into_iter()
                        .filter_map(|e| e.reason)
                        .collect();
                    (id, CalendarBusy { busy, errors })
                })
                .collect(),
        })
    }
}
