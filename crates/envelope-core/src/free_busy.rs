//! When people are busy, for scheduling meetings: asked from the service
//! of the calendar the meeting goes on (Google, Microsoft 365, Exchange),
//! and from the local calendars for the user's own addresses.

use chrono::{DateTime, Utc};
use envelope_ical::{EventStatus, PartStat};
use serde::Serialize;

use crate::db::calendar as cdb;
use crate::engine::Engine;
use crate::error::{Error, Result};

/// A busy period, in unix milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Busy {
    pub start: i64,
    pub end: i64,
    /// Not yet answered or tentatively accepted.
    pub tentative: bool,
}

/// One person's availability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Availability {
    pub email: String,
    /// `None` when their calendar could not be seen (outside the
    /// organization, or a calendar service without free/busy).
    pub busy: Option<Vec<Busy>>,
}

/// The longest window asked about at once.
const MAX_WINDOW_MS: i64 = 42 * 24 * 3600 * 1000;

fn instant(ms: i64) -> Result<DateTime<Utc>> {
    DateTime::from_timestamp_millis(ms).ok_or_else(|| Error::Invalid("bad time".into()))
}

/// Overlapping or touching periods merged, sorted by start.
fn merged(mut busy: Vec<Busy>) -> Vec<Busy> {
    busy.sort_by_key(|b| (b.start, b.end));
    let mut out: Vec<Busy> = Vec::new();
    for b in busy {
        match out.last_mut() {
            Some(last) if b.start <= last.end && b.tentative == last.tentative => {
                last.end = last.end.max(b.end);
            }
            _ => out.push(b),
        }
    }
    out
}

impl Engine {
    /// When `emails` are busy between `from` and `to`, as known to the
    /// service of `calendar_id` (the calendar the meeting is planned on).
    pub async fn free_busy(
        &self,
        calendar_id: i64,
        emails: Vec<String>,
        from: i64,
        to: i64,
    ) -> Result<Vec<Availability>> {
        if to <= from || to - from > MAX_WINDOW_MS {
            return Err(Error::Invalid("Choose a shorter time range.".into()));
        }
        let me = self.my_emails()?;
        let mut answers: Vec<Availability> = emails
            .iter()
            .map(|e| Availability {
                email: e.clone(),
                busy: None,
            })
            .collect();

        // The user's own addresses: their calendars are right here.
        let own: Vec<usize> = (0..answers.len())
            .filter(|&i| me.contains(&answers[i].email.to_lowercase()))
            .collect();
        if !own.is_empty() {
            // Subscribed calendars (holidays, sports) say nothing about the user's time.
            let subscribed: std::collections::HashSet<i64> = self
                .db
                .read(cdb::list_calendars)?
                .into_iter()
                .filter(|c| c.kind == "subscription")
                .map(|c| c.id)
                .collect();
            let busy: Vec<Busy> = self
                .occurrences(from, to)?
                .into_iter()
                .filter(|o| {
                    !subscribed.contains(&o.calendar_id)
                        && !o.transparent
                        && o.status != EventStatus::Cancelled
                        && o.my_status != Some(PartStat::Declined)
                })
                .map(|o| Busy {
                    start: o.start,
                    end: o.end,
                    tentative: matches!(
                        o.my_status,
                        Some(PartStat::Tentative | PartStat::NeedsAction)
                    ) || o.status == EventStatus::Tentative,
                })
                .collect();
            let busy = merged(busy);
            for i in own {
                answers[i].busy = Some(busy.clone());
            }
        }

        let others: Vec<String> = answers
            .iter()
            .filter(|a| a.busy.is_none())
            .map(|a| a.email.clone())
            .collect();
        if others.is_empty() {
            return Ok(answers);
        }
        let calendar = self.db.read(|c| cdb::get_calendar(c, calendar_id))?;
        let (start, end) = (instant(from)?, instant(to)?);
        let mut found: Vec<(String, Vec<Busy>)> = Vec::new();
        match calendar.kind.as_str() {
            "google" => {
                let account = calendar
                    .account_id
                    .ok_or_else(|| Error::NotFound("account".into()))?;
                let client = crate::sync::calendar::google_client(self, account)?;
                let result = client
                    .free_busy(&envelope_google::FreeBusyRequest {
                        time_min: start,
                        time_max: end,
                        calendar_ids: others.clone(),
                    })
                    .await
                    .map_err(crate::sync::calendar::google_err)?;
                for (email, cal) in result.calendars {
                    if cal.errors.is_empty() {
                        let busy = cal
                            .busy
                            .iter()
                            .map(|b| Busy {
                                start: b.start.timestamp_millis(),
                                end: b.end.timestamp_millis(),
                                tentative: false,
                            })
                            .collect();
                        found.push((email, busy));
                    }
                }
            }
            "graph" => {
                use envelope_graph::FreeBusyStatus as S;
                let account = calendar
                    .account_id
                    .ok_or_else(|| Error::NotFound("account".into()))?;
                let client = crate::sync::calendar::graph_client(self, account)?;
                let window = envelope_graph::TimeWindow { start, end };
                for chunk in others.chunks(20) {
                    let schedules = client
                        .get_schedule(chunk, &window, 30)
                        .await
                        .map_err(crate::sync::calendar::graph_err)?;
                    for s in schedules {
                        if s.error.is_some() {
                            continue;
                        }
                        let busy = s
                            .schedule_items
                            .iter()
                            .filter(|i| {
                                !matches!(i.status, S::Free | S::Unknown | S::WorkingElsewhere)
                            })
                            .filter_map(|i| {
                                Some(Busy {
                                    start: i.start.to_utc()?.timestamp_millis(),
                                    end: i.end.to_utc()?.timestamp_millis(),
                                    tentative: i.status == S::Tentative,
                                })
                            })
                            .collect();
                        found.push((s.schedule_id, busy));
                    }
                }
            }
            "ews" => {
                use envelope_ews::BusyKind;
                let account = self.account_row(
                    calendar
                        .account_id
                        .ok_or_else(|| Error::NotFound("account".into()))?,
                )?;
                let mail = crate::sync::ews::EwsMail::for_account(self, &account).await?;
                for chunk in others.chunks(100) {
                    let answers = mail
                        .client()
                        .availability(chunk, start, end)
                        .await
                        .map_err(crate::sync::ews::ews_error)?;
                    for (email, answer) in chunk.iter().zip(answers) {
                        if let Ok(periods) = answer {
                            let busy = periods
                                .iter()
                                .map(|p| Busy {
                                    start: p.start.timestamp_millis(),
                                    end: p.end.timestamp_millis(),
                                    tentative: p.kind == BusyKind::Tentative,
                                })
                                .collect();
                            found.push((email.clone(), busy));
                        }
                    }
                }
            }
            // CalDAV and local calendars: nobody else's calendar to ask.
            _ => {}
        }
        for (email, busy) in found {
            if let Some(a) = answers
                .iter_mut()
                .find(|a| a.busy.is_none() && a.email.eq_ignore_ascii_case(&email))
            {
                a.busy = Some(merged(busy));
            }
        }
        Ok(answers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_overlapping_periods() {
        let b = |start, end, tentative| Busy {
            start,
            end,
            tentative,
        };
        assert_eq!(
            merged(vec![
                b(30, 40, false),
                b(0, 10, false),
                b(5, 20, false),
                b(20, 25, false),
                b(50, 60, true)
            ]),
            [b(0, 25, false), b(30, 40, false), b(50, 60, true)]
        );
    }
}
