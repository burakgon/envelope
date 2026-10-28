//! Error types: transport and HTTP failures, SOAP faults and per-item EWS
//! response errors, with helpers that classify them for retry and resync
//! decisions.

use std::fmt;
use std::time::Duration;

use thiserror::Error;

use crate::xml::Element;

/// Convenience alias for results produced by this crate.
pub type Result<T, E = EwsError> = std::result::Result<T, E>;

macro_rules! response_codes {
    ($($(#[$doc:meta])* $variant:ident),+ $(,)?) => {
        /// An EWS `ResponseCode`.
        ///
        /// The codes a mail and calendar client has to react to are
        /// variants of their own; every other code is kept verbatim in
        /// [`ResponseCode::Other`]. The wire name of a variant is the variant
        /// name itself (`ErrorItemNotFound`, ...).
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        pub enum ResponseCode {
            $($(#[$doc])* $variant,)+
            /// Any other response code.
            Other(String),
        }

        impl ResponseCode {
            /// Parses a response code (surrounding whitespace and a namespace
            /// prefix such as `a:` are ignored).
            pub fn parse(code: &str) -> ResponseCode {
                let code = code.trim();
                let code = code.rsplit(':').next().unwrap_or(code);
                match code {
                    $(stringify!($variant) => ResponseCode::$variant,)+
                    other => ResponseCode::Other(other.to_owned()),
                }
            }

            /// The wire name of the code.
            pub fn as_str(&self) -> &str {
                match self {
                    $(ResponseCode::$variant => stringify!($variant),)+
                    ResponseCode::Other(code) => code,
                }
            }
        }
    };
}

response_codes! {
    /// Success.
    NoError,
    /// The caller lacks the rights for the operation.
    ErrorAccessDenied,
    /// The account is disabled.
    ErrorAccountDisabled,
    /// A batch was aborted after an earlier item failed.
    ErrorBatchProcessingStopped,
    /// A cancelled meeting cannot be accepted.
    ErrorCalendarIsCancelledForAccept,
    /// The occurrence was deleted from its series.
    ErrorCalendarOccurrenceIsDeletedFromRecurrence,
    /// A date is outside the range the calendar supports.
    ErrorCalendarOutOfRange,
    /// The calendar view spans too long a period.
    ErrorCalendarViewRangeTooBig,
    /// The item cannot be deleted.
    ErrorCannotDeleteObject,
    /// A change key is required for the update.
    ErrorChangeKeyRequired,
    /// A change key is required for write operations.
    ErrorChangeKeyRequiredForWriteOperations,
    /// The server could not reach the mailbox server.
    ErrorConnectionFailed,
    /// Distinguished folders cannot be deleted.
    ErrorDeleteDistinguishedFolder,
    /// Too many concurrent connections for the account.
    ErrorExceededConnectionCount,
    /// Too many subscriptions for the account.
    ErrorExceededSubscriptionCount,
    /// A find request asked for more items than the server allows at once.
    ErrorExceededFindCountLimit,
    /// The notification subscription expired.
    ErrorExpiredSubscription,
    /// A folder with that name already exists.
    ErrorFolderExists,
    /// The folder does not exist.
    ErrorFolderNotFound,
    /// The account may not impersonate the requested user.
    ErrorImpersonateUserDenied,
    /// Impersonation is not allowed.
    ErrorImpersonationDenied,
    /// An internal server error.
    ErrorInternalServerError,
    /// A transient internal server error.
    ErrorInternalServerTransientError,
    /// The attachment id is not valid.
    ErrorInvalidAttachmentId,
    /// The change key is not valid.
    ErrorInvalidChangeKey,
    /// The id is malformed.
    ErrorInvalidIdMalformed,
    /// The operation is not valid for the item.
    ErrorInvalidOperation,
    /// A requested property cannot be used with the operation.
    ErrorInvalidPropertyRequest,
    /// One or more recipients are invalid.
    ErrorInvalidRecipients,
    /// The request is invalid.
    ErrorInvalidRequest,
    /// The `RequestServerVersion` is not supported by the server.
    ErrorInvalidServerVersion,
    /// An SMTP address is invalid.
    ErrorInvalidSmtpAddress,
    /// The subscription is no longer valid.
    ErrorInvalidSubscription,
    /// The synchronization state is invalid; a full resync is required.
    ErrorInvalidSyncStateData,
    /// The update conflicts with a newer version of the item.
    ErrorIrresolvableConflict,
    /// The item does not exist (any more).
    ErrorItemNotFound,
    /// The item could not be saved.
    ErrorItemSave,
    /// The mailbox is being moved to another database.
    ErrorMailboxMoveInProgress,
    /// The mailbox database is temporarily unavailable.
    ErrorMailboxStoreUnavailable,
    /// The message exceeds the size limit.
    ErrorMessageSizeExceeded,
    /// Several directory entries matched a name to resolve.
    ErrorNameResolutionMultipleResults,
    /// No directory entry matched a name to resolve.
    ErrorNameResolutionNoResults,
    /// The mailbox does not exist.
    ErrorNonExistentMailbox,
    /// The mailbox quota is exceeded.
    ErrorQuotaExceeded,
    /// The request does not conform to the schema of the requested version.
    ErrorSchemaValidation,
    /// The account may not send as the given sender.
    ErrorSendAsDenied,
    /// The server is busy (throttling); retry after the indicated delay.
    ErrorServerBusy,
    /// The item changed since it was read.
    ErrorStaleObject,
    /// The subscription does not exist.
    ErrorSubscriptionNotFound,
    /// The synchronized folder does not exist.
    ErrorSyncFolderNotFound,
    /// The operation timed out on the server.
    ErrorTimeoutExpired,
    /// Too many objects are open on the mailbox server.
    ErrorTooManyObjectsOpened,
}

impl ResponseCode {
    /// True for codes meaning that an item or folder does not exist.
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            ResponseCode::ErrorItemNotFound
                | ResponseCode::ErrorFolderNotFound
                | ResponseCode::ErrorSyncFolderNotFound
                | ResponseCode::ErrorCalendarOccurrenceIsDeletedFromRecurrence
        )
    }

    /// True for throttling codes: the request was refused because the
    /// account or server is over a budget.
    pub fn is_throttling(&self) -> bool {
        matches!(
            self,
            ResponseCode::ErrorServerBusy
                | ResponseCode::ErrorExceededConnectionCount
                | ResponseCode::ErrorTooManyObjectsOpened
        )
    }

    /// True for codes that may succeed when the same request is repeated
    /// later.
    pub fn is_transient(&self) -> bool {
        self.is_throttling()
            || matches!(
                self,
                ResponseCode::ErrorInternalServerTransientError
                    | ResponseCode::ErrorMailboxStoreUnavailable
                    | ResponseCode::ErrorMailboxMoveInProgress
                    | ResponseCode::ErrorTimeoutExpired
                    | ResponseCode::ErrorConnectionFailed
            )
    }

    /// True when a stored `SyncState` is no longer accepted and the folder
    /// must be synchronized again from scratch.
    pub fn requires_resync(&self) -> bool {
        matches!(self, ResponseCode::ErrorInvalidSyncStateData)
    }

    /// True when a notification subscription is gone and must be created
    /// again.
    pub fn is_subscription_gone(&self) -> bool {
        matches!(
            self,
            ResponseCode::ErrorInvalidSubscription
                | ResponseCode::ErrorSubscriptionNotFound
                | ResponseCode::ErrorExpiredSubscription
        )
    }

    /// True for permission and account errors (as opposed to wrong
    /// credentials, which surface as [`EwsError::Unauthorized`]).
    pub fn is_access_denied(&self) -> bool {
        matches!(
            self,
            ResponseCode::ErrorAccessDenied
                | ResponseCode::ErrorAccountDisabled
                | ResponseCode::ErrorImpersonateUserDenied
                | ResponseCode::ErrorImpersonationDenied
                | ResponseCode::ErrorSendAsDenied
        )
    }

    /// True for an edit conflict: the item changed on the server.
    pub fn is_conflict(&self) -> bool {
        matches!(
            self,
            ResponseCode::ErrorIrresolvableConflict
                | ResponseCode::ErrorStaleObject
                | ResponseCode::ErrorInvalidChangeKey
        )
    }
}

impl fmt::Display for ResponseCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A response message with `ResponseClass="Error"`.
///
/// Batched operations (getting, updating, moving or deleting several items)
/// report one of these per failed item; single-item operations return it as
/// [`EwsError::Response`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseError {
    /// The response code.
    pub code: ResponseCode,
    /// `MessageText`: a human-readable description.
    pub message: String,
    /// `DescriptiveLinkKey`, when present.
    pub descriptive_link_key: Option<i32>,
    /// Name/value pairs from `MessageXml` (for example `BackOffMilliseconds`,
    /// or the property paths of a schema violation).
    pub details: Vec<(String, String)>,
    /// How long the server asked the client to wait (`ErrorServerBusy`).
    pub back_off: Option<Duration>,
}

impl ResponseError {
    /// Builds an error from a code and message.
    pub fn new(code: ResponseCode, message: impl Into<String>) -> Self {
        ResponseError {
            code,
            message: message.into(),
            descriptive_link_key: None,
            details: Vec::new(),
            back_off: None,
        }
    }

    /// Reads `ResponseCode`, `MessageText`, `DescriptiveLinkKey` and
    /// `MessageXml` from a response message element.
    pub(crate) fn from_message(message: &Element) -> Self {
        let code = message
            .child_text("ResponseCode")
            .map(|c| ResponseCode::parse(&c))
            .unwrap_or_else(|| ResponseCode::Other("ErrorUnknown".into()));
        let details = message
            .child("MessageXml")
            .map(message_xml_details)
            .unwrap_or_default();
        ResponseError {
            code,
            message: message.child_text("MessageText").unwrap_or_default(),
            descriptive_link_key: message.child_i32("DescriptiveLinkKey"),
            back_off: back_off_from(&details),
            details,
        }
    }

    /// A value from [`details`](Self::details) by name (case-insensitive).
    pub fn detail(&self, name: &str) -> Option<&str> {
        self.details
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

impl fmt::Display for ResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Exchange returned {}", self.code)?;
        if !self.message.is_empty() {
            write!(f, ": {}", self.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for ResponseError {}

/// A SOAP fault: the whole request was rejected (schema violations, a bad
/// `RequestServerVersion`, throttling of the request as a whole, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoapFault {
    /// HTTP status of the response carrying the fault (usually 500).
    pub http_status: u16,
    /// `faultcode` without its namespace prefix (`ErrorSchemaValidation`,
    /// `Client`, ...).
    pub fault_code: String,
    /// `faultstring`.
    pub fault_string: String,
    /// The EWS response code from the fault detail, or the fault code when
    /// it names one.
    pub code: Option<ResponseCode>,
    /// The message from the fault detail.
    pub message: Option<String>,
    /// Name/value pairs from the detail's `MessageXml`.
    pub details: Vec<(String, String)>,
    /// How long the server asked the client to wait (`ErrorServerBusy`).
    pub back_off: Option<Duration>,
}

impl SoapFault {
    /// Parses a `Fault` element.
    pub(crate) fn from_element(http_status: u16, fault: &Element) -> Self {
        let fault_code = fault
            .child_text("faultcode")
            .map(|code| code.rsplit(':').next().unwrap_or(&code).to_owned())
            .unwrap_or_default();
        let detail = fault.child("detail");
        let mut code = detail
            .and_then(|d| d.child_text("ResponseCode"))
            .map(|c| ResponseCode::parse(&c));
        if code.is_none() && fault_code.starts_with("Error") {
            code = Some(ResponseCode::parse(&fault_code));
        }
        let details = detail
            .and_then(|d| d.child("MessageXml"))
            .map(message_xml_details)
            .unwrap_or_default();
        SoapFault {
            http_status,
            fault_string: fault.child_text("faultstring").unwrap_or_default(),
            fault_code,
            code,
            message: detail.and_then(|d| d.child_text("Message")),
            back_off: back_off_from(&details),
            details,
        }
    }
}

impl fmt::Display for SoapFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self
            .code
            .as_ref()
            .map(ResponseCode::as_str)
            .unwrap_or(&self.fault_code);
        write!(f, "Exchange rejected the request ({code})")?;
        let text = self.message.as_deref().unwrap_or(&self.fault_string);
        if !text.is_empty() {
            write!(f, ": {text}")?;
        }
        Ok(())
    }
}

impl std::error::Error for SoapFault {}

/// Everything that can go wrong while talking to Exchange.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EwsError {
    /// The server rejected the credentials (HTTP 401 after the automatic
    /// retries: NTLM handshake, refreshed bearer token).
    #[error("authentication failed: the server rejected the credentials{}", Detail(.detail))]
    Unauthorized {
        /// What the server offered or why the handshake failed, if known.
        detail: Option<String>,
    },

    /// The [`TokenSource`](crate::TokenSource) could not provide an access token.
    #[error("could not obtain an access token: {0}")]
    TokenSource(String),

    /// The NTLM exchange could not be completed (malformed challenge, server
    /// did not offer NTLM, ...).
    #[error("NTLM authentication failed: {0}")]
    Ntlm(String),

    /// HTTP 403: the account may not use EWS (or the URL is blocked).
    #[error("access to {url} is forbidden")]
    Forbidden {
        /// The request URL.
        url: String,
    },

    /// A response message with `ResponseClass="Error"`.
    #[error(transparent)]
    Response(Box<ResponseError>),

    /// A SOAP fault rejecting the request as a whole.
    #[error(transparent)]
    Fault(Box<SoapFault>),

    /// Any other unexpected HTTP status.
    #[error("unexpected HTTP status {status} from {url}")]
    Http {
        /// The HTTP status code.
        status: u16,
        /// The request URL.
        url: String,
        /// The beginning of the response body, for diagnostics.
        body: String,
        /// `Retry-After`, when the server sent one.
        retry_after: Option<Duration>,
    },

    /// The request timed out.
    #[error("request timed out")]
    Timeout(#[source] reqwest::Error),

    /// Connection, TLS or protocol level failure.
    #[error("network error: {0}")]
    Transport(#[source] reqwest::Error),

    /// The response body is not well-formed XML.
    #[error("malformed XML in server response: {0}")]
    Xml(String),

    /// The response is well-formed but not what the operation expects.
    #[error("unexpected response from Exchange: {0}")]
    InvalidResponse(String),

    /// The caller supplied an argument the client cannot use.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// Redirects did not converge.
    #[error("too many redirects while requesting {url}")]
    TooManyRedirects {
        /// The URL of the original request.
        url: String,
    },

    /// Autodiscover found no EWS endpoint.
    #[error("autodiscover found no EWS endpoint for {email}{}", Attempts(.attempts))]
    AutodiscoverFailed {
        /// The address that was looked up.
        email: String,
        /// One line per endpoint that was tried and why it failed.
        attempts: Vec<String>,
        /// Whether at least one endpoint rejected the credentials.
        unauthorized: bool,
    },
}

impl EwsError {
    /// The EWS response code of a response error or SOAP fault.
    pub fn response_code(&self) -> Option<&ResponseCode> {
        match self {
            EwsError::Response(e) => Some(&e.code),
            EwsError::Fault(f) => f.code.as_ref(),
            _ => None,
        }
    }

    /// The HTTP status associated with the error, when there is one.
    pub fn status(&self) -> Option<u16> {
        match self {
            EwsError::Unauthorized { .. } => Some(401),
            EwsError::Forbidden { .. } => Some(403),
            EwsError::Fault(f) => Some(f.http_status),
            EwsError::Http { status, .. } => Some(*status),
            EwsError::Transport(e) | EwsError::Timeout(e) => e.status().map(|s| s.as_u16()),
            _ => None,
        }
    }

    /// True when the credentials were rejected or could not be obtained;
    /// the user has to sign in again.
    /// For [`EwsError::AutodiscoverFailed`] this holds when an endpoint
    /// rejected the credentials.
    pub fn is_auth_error(&self) -> bool {
        matches!(
            self,
            EwsError::Unauthorized { .. }
                | EwsError::TokenSource(_)
                | EwsError::Ntlm(_)
                | EwsError::AutodiscoverFailed {
                    unauthorized: true,
                    ..
                }
        )
    }

    /// True for HTTP 401 after the automatic retries.
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, EwsError::Unauthorized { .. })
    }

    /// True when the server throttled the request (`ErrorServerBusy`,
    /// HTTP 429, or HTTP 503 with `Retry-After`).
    pub fn is_throttled(&self) -> bool {
        match self {
            EwsError::Http {
                status,
                retry_after,
                ..
            } => *status == 429 || (*status == 503 && retry_after.is_some()),
            _ => self
                .response_code()
                .is_some_and(ResponseCode::is_throttling),
        }
    }

    /// How long the server asked the client to wait before retrying.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            EwsError::Response(e) => e.back_off,
            EwsError::Fault(f) => f.back_off,
            EwsError::Http { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// True for failures that may succeed when retried later unchanged
    /// (network problems, timeouts, throttling, 5xx responses and
    /// transient EWS codes).
    pub fn is_transient(&self) -> bool {
        match self {
            EwsError::Transport(_) | EwsError::Timeout(_) => true,
            EwsError::Http { status, .. } => {
                *status == 429 || (*status >= 500 && *status != 501 && *status != 505)
            }
            _ => self.response_code().is_some_and(ResponseCode::is_transient),
        }
    }

    /// True when the item or folder does not exist.
    pub fn is_not_found(&self) -> bool {
        self.response_code().is_some_and(ResponseCode::is_not_found)
    }

    /// True when a stored `SyncState` is no longer valid: the caller should
    /// drop it, synchronize the folder from scratch and reconcile.
    pub fn requires_resync(&self) -> bool {
        self.response_code()
            .is_some_and(ResponseCode::requires_resync)
    }

    pub(crate) fn from_reqwest(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            EwsError::Timeout(err)
        } else {
            EwsError::Transport(err)
        }
    }
}

impl From<ResponseError> for EwsError {
    fn from(value: ResponseError) -> Self {
        EwsError::Response(Box::new(value))
    }
}

impl From<SoapFault> for EwsError {
    fn from(value: SoapFault) -> Self {
        EwsError::Fault(Box::new(value))
    }
}

/// Collects the name/value pairs of a `MessageXml` element: `<t:Value
/// Name="BackOffMilliseconds">500</t:Value>` entries by their `Name`, any
/// other child element by its local name.
fn message_xml_details(message_xml: &Element) -> Vec<(String, String)> {
    message_xml
        .elements()
        .map(|el| {
            let name = match (el.name.as_str(), el.attr("Name")) {
                ("Value", Some(name)) => name.to_owned(),
                _ => el.name.clone(),
            };
            (name, el.trimmed_text().unwrap_or_default())
        })
        .collect()
}

fn back_off_from(details: &[(String, String)]) -> Option<Duration> {
    details
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("BackOffMilliseconds"))
        .and_then(|(_, value)| value.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
}

/// Formats an optional detail as `: detail`.
struct Detail<'a>(&'a Option<String>);

impl fmt::Display for Detail<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(detail) => write!(f, " ({detail})"),
            None => Ok(()),
        }
    }
}

/// Formats autodiscover attempts as `: a; b; c`.
struct Attempts<'a>(&'a [String]);

impl fmt::Display for Attempts<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            Ok(())
        } else {
            write!(f, ": {}", self.0.join("; "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml;

    #[test]
    fn parses_codes_and_round_trips_names() {
        assert_eq!(
            ResponseCode::parse("ErrorItemNotFound"),
            ResponseCode::ErrorItemNotFound
        );
        assert_eq!(
            ResponseCode::parse(" a:ErrorServerBusy "),
            ResponseCode::ErrorServerBusy
        );
        let other = ResponseCode::parse("ErrorSomethingNew");
        assert_eq!(other, ResponseCode::Other("ErrorSomethingNew".into()));
        assert_eq!(other.as_str(), "ErrorSomethingNew");
        assert_eq!(
            ResponseCode::ErrorFolderExists.to_string(),
            "ErrorFolderExists"
        );
    }

    #[test]
    fn classifies_codes() {
        assert!(ResponseCode::ErrorItemNotFound.is_not_found());
        assert!(ResponseCode::ErrorServerBusy.is_throttling());
        assert!(ResponseCode::ErrorServerBusy.is_transient());
        assert!(ResponseCode::ErrorMailboxStoreUnavailable.is_transient());
        assert!(!ResponseCode::ErrorItemNotFound.is_transient());
        assert!(ResponseCode::ErrorInvalidSyncStateData.requires_resync());
        assert!(ResponseCode::ErrorExpiredSubscription.is_subscription_gone());
        assert!(ResponseCode::ErrorImpersonateUserDenied.is_access_denied());
        assert!(ResponseCode::ErrorIrresolvableConflict.is_conflict());
    }

    #[test]
    fn reads_response_message_errors_with_back_off() {
        let message = xml::parse(
            r#"<m:GetItemResponseMessage ResponseClass="Error" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
                 <m:MessageText>The server cannot service this request right now. Try again later.</m:MessageText>
                 <m:ResponseCode>ErrorServerBusy</m:ResponseCode>
                 <m:DescriptiveLinkKey>0</m:DescriptiveLinkKey>
                 <m:MessageXml><t:Value Name="BackOffMilliseconds">1500</t:Value></m:MessageXml>
                 <m:Items/>
               </m:GetItemResponseMessage>"#,
        )
        .expect("parse");
        let error = ResponseError::from_message(&message);
        assert_eq!(error.code, ResponseCode::ErrorServerBusy);
        assert_eq!(error.descriptive_link_key, Some(0));
        assert_eq!(error.back_off, Some(Duration::from_millis(1500)));
        assert_eq!(error.detail("backoffmilliseconds"), Some("1500"));
        let err = EwsError::from(error);
        assert!(err.is_throttled());
        assert!(err.is_transient());
        assert_eq!(err.retry_after(), Some(Duration::from_millis(1500)));
        assert!(err.to_string().contains("ErrorServerBusy"));
    }

    #[test]
    fn reads_soap_faults() {
        let fault = xml::parse(
            r#"<s:Fault xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
                 <faultcode xmlns:a="http://schemas.microsoft.com/exchange/services/2006/types">a:ErrorSchemaValidation</faultcode>
                 <faultstring xml:lang="en-US">The request failed schema validation: The element 'Folders' has invalid child element 'Foo'.</faultstring>
                 <detail>
                   <e:ResponseCode xmlns:e="http://schemas.microsoft.com/exchange/services/2006/errors">ErrorSchemaValidation</e:ResponseCode>
                   <e:Message xmlns:e="http://schemas.microsoft.com/exchange/services/2006/errors">The request failed schema validation.</e:Message>
                   <t:MessageXml xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
                     <t:LineNumber>2</t:LineNumber>
                     <t:LinePosition>630</t:LinePosition>
                     <t:Violation>The element 'Folders' has invalid child element 'Foo'.</t:Violation>
                   </t:MessageXml>
                 </detail>
               </s:Fault>"#,
        )
        .expect("parse");
        let fault = SoapFault::from_element(500, &fault);
        assert_eq!(fault.fault_code, "ErrorSchemaValidation");
        assert_eq!(fault.code, Some(ResponseCode::ErrorSchemaValidation));
        assert_eq!(
            fault.message.as_deref(),
            Some("The request failed schema validation.")
        );
        assert_eq!(fault.details[0], ("LineNumber".to_owned(), "2".to_owned()));
        assert_eq!(fault.back_off, None);
        let err = EwsError::from(fault);
        assert_eq!(err.status(), Some(500));
        assert!(!err.is_transient());
        assert_eq!(
            err.to_string(),
            "Exchange rejected the request (ErrorSchemaValidation): The request failed schema validation."
        );
    }

    #[test]
    fn fault_code_names_the_error_without_detail() {
        let fault = xml::parse(
            r#"<Fault><faultcode>a:ErrorServerBusy</faultcode><faultstring>busy</faultstring></Fault>"#,
        )
        .expect("parse");
        let fault = SoapFault::from_element(500, &fault);
        assert_eq!(fault.code, Some(ResponseCode::ErrorServerBusy));
        let generic = xml::parse("<Fault><faultcode>s:Client</faultcode></Fault>").expect("parse");
        assert_eq!(SoapFault::from_element(500, &generic).code, None);
    }

    #[test]
    fn classification_helpers() {
        let http = |status: u16, retry_after: Option<Duration>| EwsError::Http {
            status,
            url: "https://mail.example.com/EWS/Exchange.asmx".into(),
            body: String::new(),
            retry_after,
        };
        assert!(http(429, None).is_throttled());
        assert!(http(503, Some(Duration::from_secs(5))).is_throttled());
        assert!(!http(503, None).is_throttled());
        assert!(http(503, None).is_transient());
        assert!(!http(501, None).is_transient());
        assert!(!http(404, None).is_transient());
        assert!(EwsError::Unauthorized { detail: None }.is_auth_error());
        assert!(EwsError::TokenSource("revoked".into()).is_auth_error());
        assert_eq!(
            EwsError::Unauthorized {
                detail: Some("server offered: Negotiate".into())
            }
            .to_string(),
            "authentication failed: the server rejected the credentials (server offered: Negotiate)"
        );
        let resync = EwsError::from(ResponseError::new(
            ResponseCode::ErrorInvalidSyncStateData,
            "Synchronization state data is corrupt or otherwise invalid.",
        ));
        assert!(resync.requires_resync());
        assert!(!resync.is_not_found());
        let failed = EwsError::AutodiscoverFailed {
            email: "jane@example.com".into(),
            attempts: vec!["a: 404".into(), "b: timeout".into()],
            unauthorized: false,
        };
        assert!(!failed.is_auth_error());
        assert_eq!(
            failed.to_string(),
            "autodiscover found no EWS endpoint for jane@example.com: a: 404; b: timeout"
        );
    }
}
