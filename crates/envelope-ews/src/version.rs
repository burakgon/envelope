//! Exchange versions: the schema version requested with
//! `RequestServerVersion` and the server build reported back in
//! `ServerVersionInfo` or by Autodiscover.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::xml::Element;

/// The EWS schema version a request is written against
/// (`RequestServerVersion`).
///
/// Newer servers accept older versions, so the default
/// ([`ExchangeVersion::Exchange2013Sp1`]) works with Exchange 2013, 2016,
/// 2019, Subscription Edition and Exchange Online. Some features need a
/// minimum version; the client leaves them out (or fails with
/// [`EwsError::InvalidInput`](crate::EwsError::InvalidInput)) when the
/// configured version is older.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[non_exhaustive]
pub enum ExchangeVersion {
    /// Exchange 2007 SP1.
    Exchange2007Sp1,
    /// Exchange 2010.
    Exchange2010,
    /// Exchange 2010 SP1.
    Exchange2010Sp1,
    /// Exchange 2010 SP2 (and SP3).
    Exchange2010Sp2,
    /// Exchange 2013.
    Exchange2013,
    /// Exchange 2013 SP1.
    #[default]
    Exchange2013Sp1,
    /// Exchange 2016 and later (2019, Subscription Edition, Exchange Online).
    Exchange2016,
}

impl ExchangeVersion {
    /// Every version, oldest first.
    pub const ALL: &'static [ExchangeVersion] = &[
        ExchangeVersion::Exchange2007Sp1,
        ExchangeVersion::Exchange2010,
        ExchangeVersion::Exchange2010Sp1,
        ExchangeVersion::Exchange2010Sp2,
        ExchangeVersion::Exchange2013,
        ExchangeVersion::Exchange2013Sp1,
        ExchangeVersion::Exchange2016,
    ];

    /// The value of `RequestServerVersion/@Version`.
    pub fn as_str(self) -> &'static str {
        match self {
            ExchangeVersion::Exchange2007Sp1 => "Exchange2007_SP1",
            ExchangeVersion::Exchange2010 => "Exchange2010",
            ExchangeVersion::Exchange2010Sp1 => "Exchange2010_SP1",
            ExchangeVersion::Exchange2010Sp2 => "Exchange2010_SP2",
            ExchangeVersion::Exchange2013 => "Exchange2013",
            ExchangeVersion::Exchange2013Sp1 => "Exchange2013_SP1",
            ExchangeVersion::Exchange2016 => "Exchange2016",
        }
    }

    /// Parses a schema version name as found in `ServerVersionInfo/@Version`
    /// (`Exchange2010_SP2`, `V2017_07_11`, `V2_68`, ...). Unknown newer
    /// names map to the newest known version.
    pub fn parse(name: &str) -> Option<ExchangeVersion> {
        let name = name.trim();
        if let Some(known) = ExchangeVersion::ALL
            .iter()
            .copied()
            .find(|v| v.as_str().eq_ignore_ascii_case(name))
        {
            return Some(known);
        }
        let upper = name.to_ascii_uppercase();
        match upper.as_str() {
            "EXCHANGE2007" => Some(ExchangeVersion::Exchange2007Sp1),
            "EXCHANGE2010_SP3" => Some(ExchangeVersion::Exchange2010Sp2),
            "EXCHANGE2012" => Some(ExchangeVersion::Exchange2013),
            "EXCHANGE2015" => Some(ExchangeVersion::Exchange2016),
            // Dated versions (`V2015_10_05`, `V2017_07_11`, ...) are 2016+.
            _ if upper.starts_with("V20") => Some(ExchangeVersion::Exchange2016),
            // Exchange Online builds of the 2013 era report `V2_nn`.
            _ if upper.starts_with("V2_") => Some(ExchangeVersion::Exchange2013Sp1),
            _ => None,
        }
    }

    /// The schema version matching a server build (`major.minor.build`).
    pub fn from_build(major: u32, minor: u32, build: u32) -> Option<ExchangeVersion> {
        match (major, minor) {
            (8, 0) => None,
            (8, _) => Some(ExchangeVersion::Exchange2007Sp1),
            (14, 0) => Some(ExchangeVersion::Exchange2010),
            (14, 1) => Some(ExchangeVersion::Exchange2010Sp1),
            (14, _) => Some(ExchangeVersion::Exchange2010Sp2),
            // Exchange 2013 SP1 is build 15.0.847.
            (15, 0) if build >= 847 => Some(ExchangeVersion::Exchange2013Sp1),
            (15, 0) => Some(ExchangeVersion::Exchange2013),
            (15, _) => Some(ExchangeVersion::Exchange2016),
            (major, _) if major > 15 => Some(ExchangeVersion::Exchange2016),
            _ => None,
        }
    }

    /// `item:Preview`, `item:Flag`, `MarkAllItemsAsRead`, `SuppressReadReceipts`.
    pub(crate) fn has_2013_features(self) -> bool {
        self >= ExchangeVersion::Exchange2013
    }

    /// `StartTimeZone`/`EndTimeZone`, `ConversationId`, `TimeZoneContext`.
    pub(crate) fn has_2010_features(self) -> bool {
        self >= ExchangeVersion::Exchange2010
    }

    /// `ReturnNewItemIds`, streaming notifications, `FreeBusyChangedEvent`.
    pub(crate) fn has_2010_sp1_features(self) -> bool {
        self >= ExchangeVersion::Exchange2010Sp1
    }

    /// Whether attendees can propose a new meeting time with their answer
    /// (`ProposedStart` and `ProposedEnd`, Exchange 2013 SP1 and later).
    pub fn has_new_time_proposals(self) -> bool {
        self >= ExchangeVersion::Exchange2013Sp1
    }
}

impl fmt::Display for ExchangeVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The server build reported in the `ServerVersionInfo` SOAP header of
/// every response.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerVersionInfo {
    /// Major version (`15` for Exchange 2013 to Subscription Edition).
    pub major_version: u32,
    /// Minor version (`0` = 2013, `1` = 2016, `2` = 2019 / SE).
    pub minor_version: u32,
    /// Major build number.
    pub major_build: u32,
    /// Minor build number.
    pub minor_build: u32,
    /// The schema version name the server reports, if any.
    pub version: Option<String>,
}

impl ServerVersionInfo {
    pub(crate) fn from_element(element: &Element) -> Option<Self> {
        let number = |name: &str| element.attr(name).and_then(|v| v.trim().parse().ok());
        Some(ServerVersionInfo {
            major_version: number("MajorVersion")?,
            minor_version: number("MinorVersion").unwrap_or(0),
            major_build: number("MajorBuildNumber").unwrap_or(0),
            minor_build: number("MinorBuildNumber").unwrap_or(0),
            version: element
                .attr("Version")
                .filter(|v| !v.trim().is_empty())
                .map(str::to_owned),
        })
    }

    /// The newest schema version the server supports, from the reported
    /// version name or, failing that, the build number.
    pub fn exchange_version(&self) -> Option<ExchangeVersion> {
        self.version
            .as_deref()
            .and_then(ExchangeVersion::parse)
            .or_else(|| {
                ExchangeVersion::from_build(
                    self.major_version,
                    self.minor_version,
                    self.major_build,
                )
            })
    }
}

impl fmt::Display for ServerVersionInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}",
            self.major_version, self.minor_version, self.major_build, self.minor_build
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml;

    #[test]
    fn version_names() {
        assert_eq!(ExchangeVersion::default().as_str(), "Exchange2013_SP1");
        for version in ExchangeVersion::ALL {
            assert_eq!(ExchangeVersion::parse(version.as_str()), Some(*version));
        }
        assert_eq!(
            ExchangeVersion::parse("V2017_07_11"),
            Some(ExchangeVersion::Exchange2016)
        );
        assert_eq!(
            ExchangeVersion::parse("V2_68"),
            Some(ExchangeVersion::Exchange2013Sp1)
        );
        assert_eq!(
            ExchangeVersion::parse("Exchange2010_SP3"),
            Some(ExchangeVersion::Exchange2010Sp2)
        );
        assert_eq!(ExchangeVersion::parse("bogus"), None);
        assert!(ExchangeVersion::Exchange2016 > ExchangeVersion::Exchange2013Sp1);
        assert!(ExchangeVersion::Exchange2013.has_2013_features());
        assert!(!ExchangeVersion::Exchange2010Sp2.has_2013_features());
        assert!(ExchangeVersion::Exchange2010Sp1.has_2010_sp1_features());
        assert!(!ExchangeVersion::Exchange2010.has_2010_sp1_features());
        assert!(ExchangeVersion::Exchange2010.has_2010_features());
        assert!(ExchangeVersion::Exchange2013Sp1.has_new_time_proposals());
        assert!(ExchangeVersion::Exchange2016.has_new_time_proposals());
        assert!(!ExchangeVersion::Exchange2013.has_new_time_proposals());
    }

    #[test]
    fn builds_map_to_versions() {
        assert_eq!(
            ExchangeVersion::from_build(15, 2, 1544),
            Some(ExchangeVersion::Exchange2016)
        );
        assert_eq!(
            ExchangeVersion::from_build(15, 0, 1497),
            Some(ExchangeVersion::Exchange2013Sp1)
        );
        assert_eq!(
            ExchangeVersion::from_build(15, 0, 516),
            Some(ExchangeVersion::Exchange2013)
        );
        assert_eq!(
            ExchangeVersion::from_build(14, 3, 123),
            Some(ExchangeVersion::Exchange2010Sp2)
        );
        assert_eq!(
            ExchangeVersion::from_build(14, 0, 1),
            Some(ExchangeVersion::Exchange2010)
        );
        assert_eq!(
            ExchangeVersion::from_build(8, 1, 1),
            Some(ExchangeVersion::Exchange2007Sp1)
        );
        assert_eq!(ExchangeVersion::from_build(6, 5, 1), None);
    }

    #[test]
    fn reads_server_version_info() {
        let el = xml::parse(
            r#"<h:ServerVersionInfo MajorVersion="15" MinorVersion="2" MajorBuildNumber="1544" MinorBuildNumber="4" Version="V2017_07_11" xmlns:h="http://schemas.microsoft.com/exchange/services/2006/types"/>"#,
        )
        .unwrap();
        let info = ServerVersionInfo::from_element(&el).unwrap();
        assert_eq!(info.to_string(), "15.2.1544.4");
        assert_eq!(info.exchange_version(), Some(ExchangeVersion::Exchange2016));
        let el = xml::parse(r#"<ServerVersionInfo MajorVersion="15" MinorVersion="0" MajorBuildNumber="1497" MinorBuildNumber="2"/>"#)
            .unwrap();
        let info = ServerVersionInfo::from_element(&el).unwrap();
        assert_eq!(info.version, None);
        assert_eq!(
            info.exchange_version(),
            Some(ExchangeVersion::Exchange2013Sp1)
        );
        assert!(
            ServerVersionInfo::from_element(&xml::parse("<ServerVersionInfo/>").unwrap()).is_none()
        );
    }
}
