//! New times proposed for meetings (iTIP `COUNTER`, RFC 5546 §3.2.7).
//!
//! An attendee proposes one from an invitation, together with their answer:
//! through Exchange for its meeting requests, through Microsoft 365 for
//! meetings on its calendars, and otherwise as an iCalendar message e-mailed
//! to the organizer. The organizer sees the proposal in their mail and
//! accepts it (the event moves, and the calendar sends the update to every
//! attendee as when it is moved by hand) or declines it (`DECLINECOUNTER`).

use std::collections::HashMap;

use chrono::{DateTime, TimeZone, Utc};
use envelope_ical::{
    Event, EventStatus, EventTime, ExpandOptions, ItipMessage, ItipMethod, PartStat, TzContext,
};
use serde::Serialize;

use super::{EditScope, invited_event, local_tz, to_ms};
use crate::db::calendar::{self as cdb, CalendarRow, EventRow};
use crate::db::mail;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::{Address, EngineEvent};
use crate::sync::calendar::{
    graph_propose, send_counter, send_decline_counter, span_text, write_respond,
};
use crate::sync::ews::calendar::{meeting_request, propose_for_request, takes_proposals};

/// Where a proposed new time stands for the organizer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProposalState {
    /// It can be accepted or declined.
    Open,
    /// The event is at the proposed time (the proposal was accepted).
    Current,
    /// The event changed after the proposal was made (it has a newer
    /// `SEQUENCE`).
    Outdated,
    /// The event, or the occurrence, is no longer on the user's calendars.
    Gone,
    /// The user can't move the event: someone else organizes it, or its
    /// calendar is read-only.
    ReadOnly,
}

/// A new time an attendee proposed for an event the user organizes, read
/// from the message carrying it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposalInfo {
    pub uid: String,
    pub summary: String,
    /// Who proposes.
    pub attendee: Address,
    /// Their answer to the invitation, sent with the proposal.
    pub status: Option<PartStat>,
    /// The proposed time (unix ms).
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    /// The event's time now, while it is on the calendar.
    pub current_start: Option<i64>,
    pub current_end: Option<i64>,
    /// The attendee's note.
    pub comment: Option<String>,
    pub event_id: Option<i64>,
    pub state: ProposalState,
}

/// The event of the user's calendars a proposal is about, as it stands.
struct Target {
    calendar: CalendarRow,
    /// The row and the occurrence to edit it with, as the calendar view
    /// does: an occurrence of a stored series goes through the series.
    event_id: i64,
    recurrence_id: Option<EventTime>,
    /// The event, the occurrence's own row, or the series an occurrence
    /// without a row of its own comes from.
    event: Event,
    /// That occurrence of `event`, for a series.
    occurrence: Option<EventTime>,
    start: i64,
    end: i64,
}

/// A proposal read from a message.
struct Proposal {
    account_id: i64,
    /// The proposal as an event: the proposed times, the proposer as its
    /// attendee.
    event: Event,
    proposer: Address,
    comment: Option<String>,
    start: i64,
    end: i64,
    target: Option<Target>,
    state: ProposalState,
}

impl Proposal {
    /// The event to act on, while the proposal can be accepted or declined.
    fn open(&self) -> Result<&Target> {
        match (&self.target, self.state) {
            (Some(target), ProposalState::Open) => Ok(target),
            (_, ProposalState::Gone) => Err(Error::NotFound(
                "The event is no longer on your calendar".into(),
            )),
            (_, ProposalState::Outdated) => Err(Error::Invalid(
                "The event has changed since this time was proposed".into(),
            )),
            (_, ProposalState::Current) => {
                Err(Error::Invalid("The event is at this time already".into()))
            }
            _ => Err(Error::Invalid(
                "Only the organizer can move this event".into(),
            )),
        }
    }

    fn summary(&self) -> &str {
        self.target
            .as_ref()
            .map(|t| t.event.summary.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&self.event.summary)
    }
}

impl Engine {
    /// Whether a new time can be proposed for `event`, the event of the
    /// invitation in message `message_id`: a single event or one occurrence
    /// (not a whole series, which Outlook and Exchange don't take proposals
    /// for, nor an all-day event), organized by someone else who hasn't
    /// ruled proposals out, through a server that can send them.
    pub(super) fn takes_proposal(
        &self,
        message_id: i64,
        itip: &ItipMessage,
        event: &Event,
        existing: &[EventRow],
        me: &[String],
    ) -> Result<bool> {
        if !matches!(itip.method, None | Some(ItipMethod::Request))
            || event.is_recurring()
            || event.is_all_day()
            || event.status == EventStatus::Cancelled
            || itip.disallows_counter()
        {
            return Ok(false);
        }
        let Some(organizer) = &event.organizer else {
            return Ok(false);
        };
        if me.contains(&organizer.email.to_lowercase()) {
            return Ok(false);
        }
        // A Microsoft 365 meeting may not take proposals.
        let refused = own_copy(existing, occurrence_ms(itip, event))
            .and_then(|row| row.provider_data.as_ref())
            .and_then(|p| p.get("allowNewTimeProposals"))
            .and_then(|v| v.as_bool())
            == Some(false);
        if refused {
            return Ok(false);
        }
        // Exchange carries them from Exchange 2013 SP1 on.
        if let Some((account, _)) = meeting_request(self, message_id)? {
            return Ok(takes_proposals(&account));
        }
        Ok(true)
    }

    /// Proposes `start`–`end` (unix ms) instead of the time of the
    /// invitation in message `message_id`, answering it at the same time as
    /// Outlook does: a decline (`status`) stays one, any other answer
    /// becomes "maybe". Returns the answer given.
    ///
    /// Exchange meeting requests and meetings on a Microsoft 365 calendar
    /// are answered through their server, which sends the proposal. For
    /// others an iCalendar `COUNTER` is e-mailed to the organizer, and the
    /// answer changes the user's copy of the event (one not on the calendar
    /// yet is put there, unless declined).
    pub async fn propose_invite_time(
        &self,
        message_id: i64,
        status: PartStat,
        start: i64,
        end: i64,
        comment: Option<String>,
    ) -> Result<PartStat> {
        let status = if status == PartStat::Declined {
            PartStat::Declined
        } else {
            PartStat::Tentative
        };
        let instant = |ms: i64| {
            Utc.timestamp_millis_opt(ms)
                .single()
                .ok_or_else(|| Error::Invalid("Invalid time".into()))
        };
        let (from, to) = (instant(start)?, instant(end)?);
        if to <= from {
            return Err(Error::Invalid("The new time ends before it starts".into()));
        }
        let comment = comment
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty());
        if !self.invite_info(message_id).await?.can_propose {
            return Err(Error::Invalid(
                "This invitation doesn't take a new time".into(),
            ));
        }
        if propose_for_request(self, message_id, status, from, to, comment.as_deref()).await? {
            return Ok(status);
        }
        let (ics, account_id) = self.invite_ics(message_id).await?;
        let itip = envelope_ical::parse_itip(&ics).map_err(|e| Error::Other(e.to_string()))?;
        let event = invited_event(&itip)?;
        let organizer = event
            .organizer
            .clone()
            .ok_or_else(|| Error::Invalid("The invitation names no organizer".into()))?;
        let existing = self.db.read(|c| cdb::rows_by_uid(c, &event.uid))?;
        let copy = own_copy(&existing, occurrence_ms(&itip, event));
        let calendar = match copy {
            Some(row) => Some(self.db.read(|c| cdb::get_calendar(c, row.calendar_id))?),
            None => None,
        };
        // A meeting on a Microsoft 365 calendar: Microsoft 365 sends it.
        if let (Some(row), Some(cal)) = (copy, &calendar)
            && cal.kind == "graph"
        {
            graph_propose(self, cal, row, status, from, to, comment.as_deref()).await?;
            self.emit(EngineEvent::CalendarChanged {
                calendar_ids: vec![cal.id],
            });
            return Ok(status);
        }
        // Otherwise by mail, to the organizer.
        let me = self.my_emails()?;
        let my_email = event
            .attendees
            .iter()
            .find(|a| me.contains(&a.email.to_lowercase()))
            .map(|a| a.email.clone())
            .unwrap_or_else(|| {
                self.account_row(account_id)
                    .map(|a| a.email)
                    .unwrap_or_default()
            });
        let counter = envelope_ical::build_counter(
            &ics,
            &my_email,
            status,
            &written_like(&itip, &event.start, from),
            &written_like(&itip, &event.start, to),
            comment.as_deref(),
        )
        .map_err(|e| Error::Other(e.to_string()))?;
        send_counter(
            self,
            account_id,
            &event.summary,
            &organizer.email,
            &span_text(start, end),
            comment.as_deref(),
            counter,
        )
        .await?;
        // The answer, on the user's calendar: the proposal carried it to
        // the organizer already.
        match (copy, &calendar) {
            (Some(row), Some(cal)) => {
                write_respond(self, cal, row, status, None, false).await?;
                self.emit(EngineEvent::CalendarChanged {
                    calendar_ids: vec![cal.id],
                });
            }
            // Nothing of it is on the calendar yet. (An occurrence of a
            // series that is there has no copy of its own to change.)
            _ if existing.is_empty() && status != PartStat::Declined => {
                self.file_invitation(account_id, &itip, &my_email, status)
                    .await?;
            }
            _ => {}
        }
        Ok(status)
    }

    /// The new time proposed in message `message_id` (an iTIP `COUNTER`)
    /// for an event the user organizes, against the event as it stands.
    pub async fn proposal_info(&self, message_id: i64) -> Result<ProposalInfo> {
        let proposal = self.read_proposal(message_id).await?;
        Ok(ProposalInfo {
            uid: proposal.event.uid.clone(),
            summary: proposal.summary().to_string(),
            attendee: proposal.proposer.clone(),
            status: proposal.event.attendees.first().map(|a| a.partstat),
            start: proposal.start,
            end: proposal.end,
            all_day: proposal.event.is_all_day(),
            current_start: proposal.target.as_ref().map(|t| t.start),
            current_end: proposal.target.as_ref().map(|t| t.end),
            comment: proposal.comment.clone(),
            event_id: proposal.target.as_ref().map(|t| t.event_id),
            state: proposal.state,
        })
    }

    /// Accepts the new time proposed in message `message_id`: the event (or
    /// the occurrence) moves there with a new `SEQUENCE`, and its calendar
    /// sends the update to every attendee, as when it is moved by hand.
    pub async fn accept_proposal(&self, message_id: i64) -> Result<()> {
        let proposal = self.read_proposal(message_id).await?;
        let target = proposal.open()?;
        let scope = if target.recurrence_id.is_some() {
            EditScope::This
        } else {
            EditScope::All
        };
        self.move_event(
            target.event_id,
            target.recurrence_id.clone(),
            scope,
            proposal.start,
            proposal.end,
        )
        .await
    }

    /// Declines the new time proposed in message `message_id`: the event
    /// stays as it is, and the attendee who proposed the time is told so (an
    /// iTIP `DECLINECOUNTER`), with the optional `comment`.
    pub async fn decline_proposal(&self, message_id: i64, comment: Option<String>) -> Result<()> {
        let proposal = self.read_proposal(message_id).await?;
        let target = proposal.open()?;
        let comment = comment
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty());
        let ics = envelope_ical::build_decline_counter(
            &target.event,
            target.occurrence.as_ref(),
            &proposal.proposer.email,
            comment.as_deref(),
        )
        .map_err(|e| Error::Other(e.to_string()))?;
        send_decline_counter(
            self,
            proposal.account_id,
            proposal.summary(),
            &proposal.proposer,
            &span_text(target.start, target.end),
            comment.as_deref(),
            ics,
        )
        .await
    }

    /// Reads the proposal carried by a message and finds its event.
    async fn read_proposal(&self, message_id: i64) -> Result<Proposal> {
        let (ics, account_id) = self.invite_ics(message_id).await?;
        let itip = envelope_ical::parse_itip(&ics).map_err(|e| Error::Other(e.to_string()))?;
        if itip.method != Some(ItipMethod::Counter) {
            return Err(Error::Invalid("This message proposes no new time".into()));
        }
        let event = itip
            .events
            .first()
            .cloned()
            .ok_or_else(|| Error::NotFound("event".into()))?;
        let ms = |t: &EventTime| envelope_ical::to_utc(t, &itip.tz, local_tz()).timestamp_millis();
        let (start, end) = (ms(&event.start), ms(&event.end));
        let target = self.proposal_target(
            &event.uid,
            event.recurrence_id.as_ref(),
            occurrence_ms(&itip, &event),
        )?;
        let from = self.db.read(|c| mail::get_message(c, message_id))?.from;
        let proposer = proposer(&event, target.as_ref(), from.as_ref())?;
        let me = self.my_emails()?;
        let state = match &target {
            None => ProposalState::Gone,
            Some(t) if t.start == start && t.end == end => ProposalState::Current,
            Some(t) if t.calendar.read_only || !organizes(&t.event, &me) => ProposalState::ReadOnly,
            Some(t) if event.sequence < t.event.sequence => ProposalState::Outdated,
            Some(_) => ProposalState::Open,
        };
        Ok(Proposal {
            account_id,
            comment: itip.comment(),
            event,
            proposer,
            start,
            end,
            target,
            state,
        })
    }

    /// The event (or the occurrence `rid`, at `rid_ms`) with `uid` on the
    /// user's calendars, preferring copies the user can change.
    fn proposal_target(
        &self,
        uid: &str,
        rid: Option<&EventTime>,
        rid_ms: Option<i64>,
    ) -> Result<Option<Target>> {
        let rows = self.db.read(|c| cdb::rows_by_uid(c, uid))?;
        let calendars: HashMap<i64, CalendarRow> = self
            .db
            .read(cdb::list_calendars)?
            .into_iter()
            .map(|c| (c.id, c))
            .collect();
        let mut rows: Vec<&EventRow> = rows
            .iter()
            .filter(|r| calendars.contains_key(&r.calendar_id))
            .collect();
        rows.sort_by_key(|r| calendars[&r.calendar_id].read_only);
        let calendar = |r: &EventRow| calendars[&r.calendar_id].clone();
        let (Some(rid), Some(rid_ms)) = (rid, rid_ms) else {
            // The event, or a whole series.
            return Ok(rows
                .iter()
                .find(|r| r.recurrence_id.is_none())
                .map(|r| Target {
                    calendar: calendar(r),
                    event_id: r.id,
                    recurrence_id: None,
                    event: r.event.clone(),
                    occurrence: None,
                    start: r.start_ts,
                    end: r.end_ts,
                }));
        };
        // The occurrence's own row: a changed occurrence of a series, or an
        // instance of a calendar that stores them one by one.
        if let Some(row) = rows
            .iter()
            .find(|r| r.recurrence_id.as_ref().map(to_ms) == Some(rid_ms))
        {
            if row.event.status == EventStatus::Cancelled {
                return Ok(None);
            }
            let cal = calendar(row);
            let series = rows
                .iter()
                .find(|m| m.calendar_id == row.calendar_id && m.is_recurring)
                .filter(|_| !cdb::stores_instances(&cal.kind));
            return Ok(Some(Target {
                calendar: cal,
                event_id: series.map_or(row.id, |m| m.id),
                recurrence_id: row.recurrence_id.clone(),
                event: row.event.clone(),
                occurrence: None,
                start: row.start_ts,
                end: row.end_ts,
            }));
        }
        // An occurrence a stored series makes.
        for series in rows.iter().filter(|r| r.is_recurring) {
            if let Some((recurrence_id, start, end)) = occurrence_of(&series.event, rid) {
                return Ok(Some(Target {
                    calendar: calendar(series),
                    event_id: series.id,
                    recurrence_id: Some(recurrence_id.clone()),
                    event: series.event.clone(),
                    occurrence: Some(recurrence_id),
                    start,
                    end,
                }));
            }
        }
        Ok(None)
    }
}

/// When the occurrence an invitation or proposal is for starts (unix ms),
/// as the message's own time zones read it; none for a whole event.
fn occurrence_ms(itip: &ItipMessage, event: &Event) -> Option<i64> {
    event
        .recurrence_id
        .as_ref()
        .map(|rid| envelope_ical::to_utc(rid, &itip.tz, local_tz()).timestamp_millis())
}

/// The user's own copy of an invited event (`rid_ms` none) or occurrence
/// among the rows with its UID.
fn own_copy(rows: &[EventRow], rid_ms: Option<i64>) -> Option<&EventRow> {
    match rid_ms {
        None => rows.iter().find(|r| r.recurrence_id.is_none()),
        Some(ms) => rows
            .iter()
            .find(|r| r.recurrence_id.as_ref().map(to_ms) == Some(ms)),
    }
}

/// `at` written the way the invitation writes `like` (its start): on the
/// clocks of its time zone when that is known, else in UTC.
fn written_like(itip: &ItipMessage, like: &EventTime, at: DateTime<Utc>) -> EventTime {
    let utc = EventTime::Utc { at };
    match like {
        EventTime::Zoned { tzid, .. } if itip.tz.resolve(tzid).is_some() => {
            itip.tz.express_like(&utc, like, local_tz())
        }
        _ => utc,
    }
}

/// The occurrence `rid` of `series`, if the series makes it: its
/// `RECURRENCE-ID` as the series writes it, and its times (unix ms).
fn occurrence_of(series: &Event, rid: &EventTime) -> Option<(EventTime, i64, i64)> {
    let ctx = TzContext::new();
    let options = ExpandOptions {
        default_tz: local_tz(),
        ..Default::default()
    };
    let at = ctx.to_utc(rid, options.default_tz);
    let window_end = at.checked_add_signed(chrono::TimeDelta::seconds(1))?;
    envelope_ical::expand(series, &[], at, window_end, &ctx, &options)
        .occurrences
        .into_iter()
        .find_map(|o| {
            let id = o.recurrence_id?;
            ctx.same_instant(&id, rid, options.default_tz)
                .then(|| (id, o.start.timestamp_millis(), o.end.timestamp_millis()))
        })
}

/// Whether the user organizes `event` (an event without an organizer is
/// the user's own).
fn organizes(event: &Event, me: &[String]) -> bool {
    event
        .organizer
        .as_ref()
        .is_none_or(|o| me.contains(&o.email.to_lowercase()))
}

/// Who proposes: the attendee of the proposal (else the sender), named as
/// the event names them.
fn proposer(proposal: &Event, target: Option<&Target>, from: Option<&Address>) -> Result<Address> {
    let Some(attendee) = proposal.attendees.first() else {
        return from
            .cloned()
            .ok_or_else(|| Error::Invalid("The proposal names nobody".into()));
    };
    let name = target
        .and_then(|t| {
            t.event
                .attendees
                .iter()
                .find(|a| a.email.eq_ignore_ascii_case(&attendee.email))
        })
        .and_then(|a| a.name.clone())
        .or_else(|| attendee.name.clone())
        .or_else(|| {
            from.filter(|f| f.email.eq_ignore_ascii_case(&attendee.email))
                .and_then(|f| f.name.clone())
        });
    Ok(Address::new(name, attendee.email.clone()))
}

#[cfg(test)]
mod tests;
