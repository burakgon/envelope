//! Automatic replies ("Out of Office"): `GetUserOofSettings` and
//! `SetUserOofSettings`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::client::{EwsClient, SoapRequest};
use crate::error::EwsError;
use crate::soap;
use crate::types::{child_datetime, format_datetime};
use crate::xml::{Element, XmlWriter};

/// Whether automatic replies are sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OofState {
    /// No automatic replies.
    #[default]
    Disabled,
    /// Automatic replies until turned off.
    Enabled,
    /// Only between [`OofSettings::start`] and [`OofSettings::end`].
    Scheduled,
}

/// Which senders outside the organization get the external reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ExternalAudience {
    /// Nobody outside the organization.
    #[default]
    None,
    /// Only senders in the user's contacts.
    Known,
    /// Every sender.
    All,
}

/// A mailbox's automatic replies.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OofSettings {
    /// Whether replies are sent.
    pub state: OofState,
    /// Who outside the organization gets [`external_reply`](Self::external_reply).
    pub external_audience: ExternalAudience,
    /// Start of a [`OofState::Scheduled`] period.
    pub start: Option<DateTime<Utc>>,
    /// End of a [`OofState::Scheduled`] period.
    pub end: Option<DateTime<Utc>>,
    /// Reply to senders inside the organization (HTML).
    pub internal_reply: String,
    /// Reply to senders outside the organization (HTML).
    pub external_reply: String,
}

fn mailbox(w: &mut XmlWriter, address: &str) {
    w.start("t:Mailbox", &[])
        .text_element("t:Address", address)
        .end("t:Mailbox");
}

fn get_request(address: &str) -> String {
    let mut w = XmlWriter::new();
    w.start("m:GetUserOofSettingsRequest", &[]);
    mailbox(&mut w, address);
    w.end("m:GetUserOofSettingsRequest");
    w.into_string()
}

fn set_request(address: &str, settings: &OofSettings) -> String {
    let state = match settings.state {
        OofState::Disabled => "Disabled",
        OofState::Enabled => "Enabled",
        OofState::Scheduled => "Scheduled",
    };
    let audience = match settings.external_audience {
        ExternalAudience::None => "None",
        ExternalAudience::Known => "Known",
        ExternalAudience::All => "All",
    };
    let mut w = XmlWriter::new();
    w.start("m:SetUserOofSettingsRequest", &[]);
    mailbox(&mut w, address);
    w.start("t:UserOofSettings", &[])
        .text_element("t:OofState", state)
        .text_element("t:ExternalAudience", audience);
    if let (Some(start), Some(end)) = (settings.start, settings.end) {
        w.start("t:Duration", &[])
            .text_element("t:StartTime", &format_datetime(start))
            .text_element("t:EndTime", &format_datetime(end))
            .end("t:Duration");
    }
    w.start("t:InternalReply", &[])
        .text_element("t:Message", &settings.internal_reply)
        .end("t:InternalReply")
        .start("t:ExternalReply", &[])
        .text_element("t:Message", &settings.external_reply)
        .end("t:ExternalReply")
        .end("t:UserOofSettings")
        .end("m:SetUserOofSettingsRequest");
    w.into_string()
}

/// These operations answer with one `ResponseMessage` instead of the usual
/// `ResponseMessages` list.
fn check(response: &Element) -> Result<(), EwsError> {
    let message = response.child("ResponseMessage").ok_or_else(|| {
        EwsError::InvalidResponse(format!("<{}> without ResponseMessage", response.name))
    })?;
    soap::check(message).map(|_| ()).map_err(EwsError::from)
}

fn parse(response: &Element) -> Result<OofSettings, EwsError> {
    check(response)?;
    let settings = response
        .child("OofSettings")
        .ok_or_else(|| EwsError::InvalidResponse("no OofSettings".into()))?;
    let reply = |name: &str| {
        settings
            .child(name)
            .and_then(|r| r.child_raw_text("Message"))
            .unwrap_or_default()
    };
    let duration = settings.child("Duration");
    Ok(OofSettings {
        state: match settings.child_text("OofState").as_deref() {
            Some("Enabled") => OofState::Enabled,
            Some("Scheduled") => OofState::Scheduled,
            _ => OofState::Disabled,
        },
        external_audience: match settings.child_text("ExternalAudience").as_deref() {
            Some("Known") => ExternalAudience::Known,
            Some("All") => ExternalAudience::All,
            _ => ExternalAudience::None,
        },
        start: duration.and_then(|d| child_datetime(d, "StartTime")),
        end: duration.and_then(|d| child_datetime(d, "EndTime")),
        internal_reply: reply("InternalReply"),
        external_reply: reply("ExternalReply"),
    })
}

impl EwsClient {
    /// The automatic replies of `mailbox` (an SMTP address).
    pub async fn oof_settings(&self, mailbox: &str) -> Result<OofSettings, EwsError> {
        let response = self
            .call(&SoapRequest::read(
                "GetUserOofSettings",
                get_request(mailbox),
            ))
            .await?;
        parse(&response)
    }

    /// Replaces the automatic replies of `mailbox`.
    pub async fn set_oof_settings(
        &self,
        mailbox: &str,
        settings: &OofSettings,
    ) -> Result<(), EwsError> {
        if settings.state == OofState::Scheduled
            && (settings.start.is_none() || settings.end.is_none())
        {
            return Err(EwsError::InvalidInput(
                "scheduled automatic replies need a start and an end".into(),
            ));
        }
        let response = self
            .call(&SoapRequest::write(
                "SetUserOofSettings",
                set_request(mailbox, settings),
            ))
            .await?;
        check(&response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml;
    use chrono::TimeZone;

    #[test]
    fn writes_the_settings() {
        let settings = OofSettings {
            state: OofState::Scheduled,
            external_audience: ExternalAudience::Known,
            start: Some(Utc.with_ymd_and_hms(2026, 11, 2, 0, 0, 0).unwrap()),
            end: Some(Utc.with_ymd_and_hms(2026, 11, 9, 0, 0, 0).unwrap()),
            internal_reply: "<p>Away & back soon</p>".into(),
            external_reply: "<p>Away</p>".into(),
        };
        assert_eq!(
            set_request("jane@example.com", &settings),
            "<m:SetUserOofSettingsRequest><t:Mailbox><t:Address>jane@example.com</t:Address></t:Mailbox>\
             <t:UserOofSettings><t:OofState>Scheduled</t:OofState><t:ExternalAudience>Known</t:ExternalAudience>\
             <t:Duration><t:StartTime>2026-11-02T00:00:00Z</t:StartTime><t:EndTime>2026-11-09T00:00:00Z</t:EndTime></t:Duration>\
             <t:InternalReply><t:Message>&lt;p&gt;Away &amp; back soon&lt;/p&gt;</t:Message></t:InternalReply>\
             <t:ExternalReply><t:Message>&lt;p&gt;Away&lt;/p&gt;</t:Message></t:ExternalReply>\
             </t:UserOofSettings></m:SetUserOofSettingsRequest>"
        );
    }

    #[test]
    fn reads_the_settings() {
        let response = xml::parse(
            r#"<GetUserOofSettingsResponse xmlns="http://schemas.microsoft.com/exchange/services/2006/messages">
                 <ResponseMessage ResponseClass="Success"><ResponseCode>NoError</ResponseCode></ResponseMessage>
                 <OofSettings xmlns="http://schemas.microsoft.com/exchange/services/2006/types">
                   <OofState>Scheduled</OofState>
                   <ExternalAudience>All</ExternalAudience>
                   <Duration><StartTime>2026-11-02T00:00:00</StartTime><EndTime>2026-11-09T00:00:00</EndTime></Duration>
                   <InternalReply><Message>&lt;p&gt;Back on Monday&lt;/p&gt;</Message></InternalReply>
                   <ExternalReply><Message></Message></ExternalReply>
                 </OofSettings>
                 <AllowExternalOof>All</AllowExternalOof>
               </GetUserOofSettingsResponse>"#,
        )
        .unwrap();
        let settings = parse(&response).unwrap();
        assert_eq!(settings.state, OofState::Scheduled);
        assert_eq!(settings.external_audience, ExternalAudience::All);
        assert_eq!(
            settings.start,
            Some(Utc.with_ymd_and_hms(2026, 11, 2, 0, 0, 0).unwrap())
        );
        assert_eq!(
            settings.end,
            Some(Utc.with_ymd_and_hms(2026, 11, 9, 0, 0, 0).unwrap())
        );
        assert_eq!(settings.internal_reply, "<p>Back on Monday</p>");
        assert_eq!(settings.external_reply, "");
    }

    #[test]
    fn reports_errors() {
        let response = xml::parse(
            r#"<GetUserOofSettingsResponse xmlns="m">
                 <ResponseMessage ResponseClass="Error"><MessageText>Access is denied.</MessageText>
                 <ResponseCode>ErrorAccessDenied</ResponseCode></ResponseMessage>
               </GetUserOofSettingsResponse>"#,
        )
        .unwrap();
        assert!(parse(&response).is_err());
    }
}
