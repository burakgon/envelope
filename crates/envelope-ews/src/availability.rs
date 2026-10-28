//! Free/busy times of other mailboxes (`GetUserAvailability`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::client::{EwsClient, SoapRequest};
use crate::error::{EwsError, ResponseError};
use crate::soap;
use crate::types::parse_datetime;
use crate::xml::{Element, XmlWriter};

/// How a busy period is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BusyKind {
    /// Tentatively accepted or not yet answered.
    Tentative,
    /// Busy.
    Busy,
    /// Out of office.
    OutOfOffice,
}

/// One busy period of a mailbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusyPeriod {
    /// Start.
    pub start: DateTime<Utc>,
    /// End.
    pub end: DateTime<Utc>,
    /// How busy.
    pub kind: BusyKind,
}

/// UTC throughout: times in the request and the answer are then plain UTC.
const UTC_ZONE: &str = "<t:TimeZone><t:Bias>0</t:Bias>\
<t:StandardTime><t:Bias>0</t:Bias><t:Time>02:00:00</t:Time><t:DayOrder>5</t:DayOrder><t:Month>10</t:Month><t:DayOfWeek>Sunday</t:DayOfWeek></t:StandardTime>\
<t:DaylightTime><t:Bias>0</t:Bias><t:Time>02:00:00</t:Time><t:DayOrder>1</t:DayOrder><t:Month>4</t:Month><t:DayOfWeek>Sunday</t:DayOfWeek></t:DaylightTime>\
</t:TimeZone>";

fn utc(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn request(addresses: &[String], start: DateTime<Utc>, end: DateTime<Utc>) -> String {
    let mut w = XmlWriter::new();
    w.start("m:GetUserAvailabilityRequest", &[]).raw(UTC_ZONE);
    w.start("m:MailboxDataArray", &[]);
    for address in addresses {
        w.start("t:MailboxData", &[])
            .start("t:Email", &[])
            .text_element("t:Address", address)
            .end("t:Email")
            .text_element("t:AttendeeType", "Required")
            .text_element("t:ExcludeConflicts", "false")
            .end("t:MailboxData");
    }
    w.end("m:MailboxDataArray")
        .start("t:FreeBusyViewOptions", &[])
        .start("t:TimeWindow", &[])
        .text_element("t:StartTime", &utc(start))
        .text_element("t:EndTime", &utc(end))
        .end("t:TimeWindow")
        .text_element("t:MergedFreeBusyIntervalInMinutes", "30")
        .text_element("t:RequestedView", "FreeBusy")
        .end("t:FreeBusyViewOptions")
        .end("m:GetUserAvailabilityRequest");
    w.into_string()
}

fn periods(view: &Element) -> Vec<BusyPeriod> {
    let Some(events) = view.child("CalendarEventArray") else {
        return Vec::new();
    };
    events
        .children_named("CalendarEvent")
        .filter_map(|e| {
            let kind = match e.child_text("BusyType").as_deref() {
                Some("Tentative") => BusyKind::Tentative,
                Some("Busy") => BusyKind::Busy,
                Some("OOF") => BusyKind::OutOfOffice,
                _ => return None,
            };
            Some(BusyPeriod {
                start: parse_datetime(&e.child_text("StartTime")?)?,
                end: parse_datetime(&e.child_text("EndTime")?)?,
                kind,
            })
        })
        .collect()
}

/// The answers in request order: busy periods, or why a mailbox could not
/// be looked up.
fn parse(response: &Element) -> Result<Vec<Result<Vec<BusyPeriod>, ResponseError>>, EwsError> {
    let array = response
        .child("FreeBusyResponseArray")
        .ok_or_else(|| EwsError::InvalidResponse("no FreeBusyResponseArray".into()))?;
    Ok(array
        .children_named("FreeBusyResponse")
        .map(|r| {
            if let Some(message) = r.child("ResponseMessage") {
                soap::check(message)?;
            }
            Ok(r.child("FreeBusyView").map(periods).unwrap_or_default())
        })
        .collect())
}

impl EwsClient {
    /// When each of `addresses` is busy between `start` and `end` (at most
    /// 42 days and 100 mailboxes per call, as Exchange allows).
    pub async fn availability(
        &self,
        addresses: &[String],
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<Result<Vec<BusyPeriod>, ResponseError>>, EwsError> {
        if addresses.is_empty() {
            return Ok(Vec::new());
        }
        if end <= start {
            return Err(EwsError::InvalidInput(
                "the free/busy window must end after it starts".into(),
            ));
        }
        let response = self
            .call(&SoapRequest::read(
                "GetUserAvailability",
                request(addresses, start, end),
            ))
            .await?;
        parse(&response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml;
    use chrono::TimeZone;

    #[test]
    fn asks_in_utc() {
        let body = request(
            &["jane@example.com".into()],
            Utc.with_ymd_and_hms(2026, 11, 10, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 11, 11, 0, 0, 0).unwrap(),
        );
        assert!(body.starts_with("<m:GetUserAvailabilityRequest><t:TimeZone><t:Bias>0</t:Bias>"));
        assert!(body.contains("<t:MailboxData><t:Email><t:Address>jane@example.com</t:Address></t:Email><t:AttendeeType>Required</t:AttendeeType>"));
        assert!(body.contains("<t:TimeWindow><t:StartTime>2026-11-10T00:00:00</t:StartTime><t:EndTime>2026-11-11T00:00:00</t:EndTime></t:TimeWindow>"));
    }

    #[test]
    fn reads_busy_periods_and_errors() {
        let response = xml::parse(
            r#"<GetUserAvailabilityResponse xmlns="m">
                 <FreeBusyResponseArray>
                   <FreeBusyResponse>
                     <ResponseMessage ResponseClass="Success"><ResponseCode>NoError</ResponseCode></ResponseMessage>
                     <FreeBusyView>
                       <FreeBusyViewType>FreeBusy</FreeBusyViewType>
                       <CalendarEventArray>
                         <CalendarEvent><StartTime>2026-11-10T09:00:00</StartTime><EndTime>2026-11-10T10:00:00</EndTime><BusyType>Busy</BusyType></CalendarEvent>
                         <CalendarEvent><StartTime>2026-11-10T13:00:00</StartTime><EndTime>2026-11-10T13:30:00</EndTime><BusyType>Tentative</BusyType></CalendarEvent>
                         <CalendarEvent><StartTime>2026-11-10T15:00:00</StartTime><EndTime>2026-11-10T16:00:00</EndTime><BusyType>Free</BusyType></CalendarEvent>
                       </CalendarEventArray>
                     </FreeBusyView>
                   </FreeBusyResponse>
                   <FreeBusyResponse>
                     <ResponseMessage ResponseClass="Error"><MessageText>Not found</MessageText>
                       <ResponseCode>ErrorMailRecipientNotFound</ResponseCode></ResponseMessage>
                   </FreeBusyResponse>
                 </FreeBusyResponseArray>
               </GetUserAvailabilityResponse>"#,
        )
        .unwrap();
        let answers = parse(&response).unwrap();
        let first = answers[0].as_ref().unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].kind, BusyKind::Busy);
        assert_eq!(
            first[0].start,
            Utc.with_ymd_and_hms(2026, 11, 10, 9, 0, 0).unwrap()
        );
        assert_eq!(first[1].kind, BusyKind::Tentative);
        assert!(answers[1].is_err());
    }
}
