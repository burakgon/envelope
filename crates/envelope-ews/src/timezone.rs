//! Windows ↔ IANA time zone mapping.
//!
//! Exchange names time zones by their Windows ids (`W. Europe Standard
//! Time`). The mapping uses the same CLDR-derived tables as the rest of
//! Envelope ([`envelope_ical::tz`]), so zones resolve identically for EWS,
//! Microsoft Graph and iCalendar data.

use chrono_tz::Tz;

pub use envelope_ical::tz::{iana_to_windows, windows_to_iana};

/// Resolves a zone id found in EWS data: a Windows id, an Outlook display
/// name (`(UTC+01:00) Amsterdam, Berlin, …`) or an IANA name.
pub fn resolve_time_zone(id: &str) -> Option<Tz> {
    envelope_ical::tz::resolve_tzid(id)
}

/// The Windows id to send for a zone; `UTC` for zones without one.
pub fn windows_zone_id(tz: Tz) -> &'static str {
    iana_to_windows(tz).unwrap_or("UTC")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_both_ways() {
        assert_eq!(
            windows_to_iana("W. Europe Standard Time"),
            Some(Tz::Europe__Berlin)
        );
        assert_eq!(
            windows_to_iana("Turkey Standard Time"),
            Some(Tz::Europe__Istanbul)
        );
        assert_eq!(
            windows_zone_id(Tz::America__New_York),
            "Eastern Standard Time"
        );
        assert_eq!(windows_zone_id(Tz::UTC), "UTC");
        assert_eq!(
            windows_zone_id(Tz::Europe__Istanbul),
            "Turkey Standard Time"
        );
        assert_eq!(
            resolve_time_zone("(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna"),
            Some(Tz::Europe__Berlin)
        );
        assert_eq!(
            resolve_time_zone("Pacific Standard Time"),
            Some(Tz::America__Los_Angeles)
        );
        assert_eq!(resolve_time_zone("Asia/Tokyo"), Some(Tz::Asia__Tokyo));
        assert_eq!(resolve_time_zone("Nowhere Standard Time"), None);
    }
}
