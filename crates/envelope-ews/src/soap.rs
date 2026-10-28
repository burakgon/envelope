//! SOAP envelopes: writing requests and taking responses apart.

use crate::error::{EwsError, ResponseError, SoapFault};
use crate::version::ServerVersionInfo;
use crate::xml::{self, Element, NS_MESSAGES, NS_SOAP, NS_TYPES, XmlWriter};

/// Headers of a request envelope.
#[derive(Debug, Clone, Default)]
pub(crate) struct SoapHeaders<'a> {
    pub(crate) version: &'a str,
    /// Primary SMTP address of the impersonated mailbox.
    pub(crate) impersonate: Option<&'a str>,
    /// Windows time zone id for `TimeZoneContext`.
    pub(crate) time_zone: Option<&'a str>,
}

/// Wraps an operation element in a SOAP envelope.
pub(crate) fn envelope(headers: &SoapHeaders<'_>, operation: &str) -> String {
    let mut w = XmlWriter::new();
    w.raw(r#"<?xml version="1.0" encoding="utf-8"?>"#);
    w.start(
        "soap:Envelope",
        &[
            ("xmlns:soap", NS_SOAP),
            ("xmlns:t", NS_TYPES),
            ("xmlns:m", NS_MESSAGES),
        ],
    );
    w.start("soap:Header", &[]);
    w.empty("t:RequestServerVersion", &[("Version", headers.version)]);
    if let Some(address) = headers.impersonate {
        w.start("t:ExchangeImpersonation", &[])
            .start("t:ConnectingSID", &[])
            .text_element("t:PrimarySmtpAddress", address)
            .end("t:ConnectingSID")
            .end("t:ExchangeImpersonation");
    }
    if let Some(zone) = headers.time_zone {
        w.start("t:TimeZoneContext", &[])
            .empty("t:TimeZoneDefinition", &[("Id", zone)])
            .end("t:TimeZoneContext");
    }
    w.end("soap:Header");
    w.start("soap:Body", &[]).raw(operation).end("soap:Body");
    w.end("soap:Envelope");
    w.into_string()
}

/// A parsed response envelope.
#[derive(Debug, Clone)]
pub(crate) struct SoapResponse {
    /// The first element inside `Body` (`m:GetItemResponse`, ...).
    pub(crate) body: Element,
    /// The `ServerVersionInfo` header, when present.
    pub(crate) server_version: Option<ServerVersionInfo>,
}

/// What a response body turned out to contain.
#[derive(Debug)]
pub(crate) enum Parsed {
    Response(SoapResponse),
    Fault(SoapFault),
}

/// Parses a response envelope. Returns `Err` for documents that are not
/// SOAP envelopes at all.
pub(crate) fn parse_envelope(http_status: u16, text: &str) -> Result<Parsed, EwsError> {
    let root = xml::parse(text)?;
    if !root.is("Envelope") {
        return Err(EwsError::InvalidResponse(format!(
            "expected a SOAP envelope, got <{}>",
            root.name
        )));
    }
    let server_version = root
        .child("Header")
        .and_then(|h| h.child("ServerVersionInfo"))
        .and_then(ServerVersionInfo::from_element);
    let body = root
        .child("Body")
        .ok_or_else(|| EwsError::InvalidResponse("SOAP envelope without Body".into()))?;
    let first = body
        .elements()
        .next()
        .ok_or_else(|| EwsError::InvalidResponse("empty SOAP Body".into()))?;
    if first.is("Fault") && matches!(first.ns, xml::Ns::Soap | xml::Ns::Unbound) {
        return Ok(Parsed::Fault(SoapFault::from_element(http_status, first)));
    }
    Ok(Parsed::Response(SoapResponse {
        body: first.clone(),
        server_version,
    }))
}

/// The response messages of an operation response (the children of
/// `ResponseMessages`).
pub(crate) fn response_messages(response: &Element) -> Result<Vec<&Element>, EwsError> {
    let messages = response.child("ResponseMessages").ok_or_else(|| {
        EwsError::InvalidResponse(format!("<{}> without ResponseMessages", response.name))
    })?;
    Ok(messages.elements().collect())
}

/// Checks the `ResponseClass` of one response message. `Success` and
/// `Warning` pass; `Error` becomes a [`ResponseError`].
pub(crate) fn check(message: &Element) -> Result<&Element, ResponseError> {
    match message.attr("ResponseClass") {
        Some(class) if class.eq_ignore_ascii_case("Error") => {
            Err(ResponseError::from_message(message))
        }
        Some(class) if class.eq_ignore_ascii_case("Warning") => {
            tracing::debug!(
                code = message.child_text("ResponseCode").unwrap_or_default(),
                text = message.child_text("MessageText").unwrap_or_default(),
                "EWS warning"
            );
            Ok(message)
        }
        _ => Ok(message),
    }
}

/// The single response message of a one-item operation, checked.
pub(crate) fn single(response: &Element) -> Result<&Element, EwsError> {
    let messages = response_messages(response)?;
    let first = messages
        .into_iter()
        .next()
        .ok_or_else(|| EwsError::InvalidResponse("no response message".into()))?;
    check(first).map_err(EwsError::from)
}

/// Every response message checked, in request order.
pub(crate) fn each(response: &Element) -> Result<Vec<Result<&Element, ResponseError>>, EwsError> {
    Ok(response_messages(response)?
        .into_iter()
        .map(check)
        .collect())
}

/// True when every response message reports `ErrorServerBusy`: the whole
/// request was throttled and can be repeated.
pub(crate) fn all_busy(response: &Element) -> Option<ResponseError> {
    let messages = response
        .child("ResponseMessages")
        .map(|m| m.elements().collect::<Vec<_>>())?;
    if messages.is_empty() {
        return None;
    }
    let mut first = None;
    for message in messages {
        let error = match check(message) {
            Err(error) if error.code == crate::ResponseCode::ErrorServerBusy => error,
            _ => return None,
        };
        first.get_or_insert(error);
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelopes_carry_version_impersonation_and_time_zone() {
        let text = envelope(
            &SoapHeaders {
                version: "Exchange2013_SP1",
                impersonate: Some("room&co@example.com"),
                time_zone: Some("W. Europe Standard Time"),
            },
            "<m:GetFolder/>",
        );
        assert!(text.starts_with(r#"<?xml version="1.0" encoding="utf-8"?><soap:Envelope"#));
        let root = xml::parse(&text).expect("well-formed");
        assert_eq!(root.ns, xml::Ns::Soap);
        let header = root.child("Header").unwrap();
        assert_eq!(
            header
                .child("RequestServerVersion")
                .and_then(|e| e.attr("Version")),
            Some("Exchange2013_SP1")
        );
        assert_eq!(
            header.child("RequestServerVersion").unwrap().ns,
            xml::Ns::Types
        );
        assert_eq!(
            header
                .path(&[
                    "ExchangeImpersonation",
                    "ConnectingSID",
                    "PrimarySmtpAddress"
                ])
                .and_then(Element::trimmed_text)
                .as_deref(),
            Some("room&co@example.com")
        );
        assert_eq!(
            header
                .path(&["TimeZoneContext", "TimeZoneDefinition"])
                .and_then(|e| e.attr("Id")),
            Some("W. Europe Standard Time")
        );
        let op = root.path(&["Body", "GetFolder"]).unwrap();
        assert_eq!(op.ns, xml::Ns::Messages);

        let plain = envelope(
            &SoapHeaders {
                version: "Exchange2010_SP2",
                ..SoapHeaders::default()
            },
            "<m:GetFolder/>",
        );
        assert!(!plain.contains("ExchangeImpersonation"));
        assert!(!plain.contains("TimeZoneContext"));
    }

    #[test]
    fn parses_responses_and_faults() {
        let ok = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Header><h:ServerVersionInfo MajorVersion="15" MinorVersion="1" MajorBuildNumber="2507" MinorBuildNumber="6" Version="V2017_07_11" xmlns:h="http://schemas.microsoft.com/exchange/services/2006/types"/></s:Header>
  <s:Body><m:DeleteItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
    <m:ResponseMessages>
      <m:DeleteItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:DeleteItemResponseMessage>
      <m:DeleteItemResponseMessage ResponseClass="Error"><m:MessageText>The specified object was not found in the store.</m:MessageText><m:ResponseCode>ErrorItemNotFound</m:ResponseCode><m:DescriptiveLinkKey>0</m:DescriptiveLinkKey></m:DeleteItemResponseMessage>
      <m:DeleteItemResponseMessage ResponseClass="Warning"><m:MessageText>w</m:MessageText><m:ResponseCode>ErrorBatchProcessingStopped</m:ResponseCode></m:DeleteItemResponseMessage>
    </m:ResponseMessages>
  </m:DeleteItemResponse></s:Body>
</s:Envelope>"#;
        let Parsed::Response(response) = parse_envelope(200, ok).unwrap() else {
            panic!("expected a response");
        };
        assert_eq!(
            response.server_version.as_ref().map(|v| v.major_build),
            Some(2507)
        );
        assert!(response.body.is("DeleteItemResponse"));
        let results = each(&response.body).unwrap();
        assert_eq!(results.len(), 3);
        assert!(results[0].is_ok());
        assert_eq!(
            results[1].as_ref().unwrap_err().code,
            crate::ResponseCode::ErrorItemNotFound
        );
        assert!(results[2].is_ok());
        assert!(single(&response.body).is_ok());
        assert!(all_busy(&response.body).is_none());

        let fault = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><s:Fault>
            <faultcode xmlns:a="http://schemas.microsoft.com/exchange/services/2006/types">a:ErrorServerBusy</faultcode>
            <faultstring xml:lang="en-US">The server cannot service this request right now. Try again later.</faultstring>
            <detail><e:ResponseCode xmlns:e="http://schemas.microsoft.com/exchange/services/2006/errors">ErrorServerBusy</e:ResponseCode>
            <t:MessageXml xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"><t:Value Name="BackOffMilliseconds">297749</t:Value></t:MessageXml></detail>
            </s:Fault></s:Body></s:Envelope>"#;
        let Parsed::Fault(fault) = parse_envelope(500, fault).unwrap() else {
            panic!("expected a fault");
        };
        assert_eq!(fault.code, Some(crate::ResponseCode::ErrorServerBusy));
        assert_eq!(
            fault.back_off,
            Some(std::time::Duration::from_millis(297_749))
        );

        assert!(matches!(
            parse_envelope(200, "<html><body>Sign in</body></html>"),
            Err(EwsError::InvalidResponse(_))
        ));
        assert!(matches!(
            parse_envelope(200, "not xml <"),
            Err(EwsError::Xml(_))
        ));
    }

    #[test]
    fn detects_whole_request_throttling() {
        let busy = xml::parse(
            r#"<m:GetItemResponse xmlns:m="m"><m:ResponseMessages>
                 <m:GetItemResponseMessage ResponseClass="Error"><m:ResponseCode>ErrorServerBusy</m:ResponseCode><m:MessageXml><t:Value xmlns:t="t" Name="BackOffMilliseconds">20</t:Value></m:MessageXml></m:GetItemResponseMessage>
                 <m:GetItemResponseMessage ResponseClass="Error"><m:ResponseCode>ErrorServerBusy</m:ResponseCode></m:GetItemResponseMessage>
               </m:ResponseMessages></m:GetItemResponse>"#,
        )
        .unwrap();
        let error = all_busy(&busy).expect("busy");
        assert_eq!(error.back_off, Some(std::time::Duration::from_millis(20)));
        let empty = xml::parse(
            "<m:GetItemResponse xmlns:m=\"m\"><m:ResponseMessages/></m:GetItemResponse>",
        )
        .unwrap();
        assert!(all_busy(&empty).is_none());
        assert!(matches!(single(&empty), Err(EwsError::InvalidResponse(_))));
    }
}
