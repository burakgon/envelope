//! Windows time zone tables.
//!
//! Generated from the Unicode CLDR `windowsZones.xml` (territory `001`,
//! i.e. the primary IANA zone for every Windows zone), including the Windows
//! display names that Outlook and Exchange sometimes use as TZIDs. CLDR's
//! legacy IANA aliases (`Asia/Calcutta`, `Europe/Kiev`, …) are replaced by
//! their canonical names.

/// One Windows zone: its registry key name, English display name and primary IANA zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowsZone {
    /// Windows zone id (registry key), e.g. `W. Europe Standard Time`.
    pub id: &'static str,
    /// English display name, e.g. `(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna`.
    pub display: &'static str,
    /// Primary IANA zone (CLDR territory `001`).
    pub iana: &'static str,
}

/// Every Windows zone known to CLDR.
pub static WINDOWS_ZONES: &[WindowsZone] = &[
    WindowsZone {
        id: "Dateline Standard Time",
        display: "(UTC-12:00) International Date Line West",
        iana: "Etc/GMT+12",
    },
    WindowsZone {
        id: "UTC-11",
        display: "(UTC-11:00) Coordinated Universal Time-11",
        iana: "Etc/GMT+11",
    },
    WindowsZone {
        id: "Aleutian Standard Time",
        display: "(UTC-10:00) Aleutian Islands",
        iana: "America/Adak",
    },
    WindowsZone {
        id: "Hawaiian Standard Time",
        display: "(UTC-10:00) Hawaii",
        iana: "Pacific/Honolulu",
    },
    WindowsZone {
        id: "Marquesas Standard Time",
        display: "(UTC-09:30) Marquesas Islands",
        iana: "Pacific/Marquesas",
    },
    WindowsZone {
        id: "Alaskan Standard Time",
        display: "(UTC-09:00) Alaska",
        iana: "America/Anchorage",
    },
    WindowsZone {
        id: "UTC-09",
        display: "(UTC-09:00) Coordinated Universal Time-09",
        iana: "Etc/GMT+9",
    },
    WindowsZone {
        id: "Pacific Standard Time (Mexico)",
        display: "(UTC-08:00) Baja California",
        iana: "America/Tijuana",
    },
    WindowsZone {
        id: "UTC-08",
        display: "(UTC-08:00) Coordinated Universal Time-08",
        iana: "Etc/GMT+8",
    },
    WindowsZone {
        id: "Pacific Standard Time",
        display: "(UTC-08:00) Pacific Time (US & Canada)",
        iana: "America/Los_Angeles",
    },
    WindowsZone {
        id: "US Mountain Standard Time",
        display: "(UTC-07:00) Arizona",
        iana: "America/Phoenix",
    },
    WindowsZone {
        id: "Mountain Standard Time (Mexico)",
        display: "(UTC-07:00) Chihuahua, La Paz, Mazatlan",
        iana: "America/Mazatlan",
    },
    WindowsZone {
        id: "Mountain Standard Time",
        display: "(UTC-07:00) Mountain Time (US & Canada)",
        iana: "America/Denver",
    },
    WindowsZone {
        id: "British Columbia Standard Time",
        display: "(UTC-07:00) British Columbia",
        iana: "America/Vancouver",
    },
    WindowsZone {
        id: "Yukon Standard Time",
        display: "(UTC-07:00) Yukon",
        iana: "America/Whitehorse",
    },
    WindowsZone {
        id: "Alberta Standard Time",
        display: "(UTC-07:00) Alberta",
        iana: "America/Edmonton",
    },
    WindowsZone {
        id: "Central America Standard Time",
        display: "(UTC-06:00) Central America",
        iana: "America/Guatemala",
    },
    WindowsZone {
        id: "Central Standard Time",
        display: "(UTC-06:00) Central Time (US & Canada)",
        iana: "America/Chicago",
    },
    WindowsZone {
        id: "Easter Island Standard Time",
        display: "(UTC-06:00) Easter Island",
        iana: "Pacific/Easter",
    },
    WindowsZone {
        id: "Central Standard Time (Mexico)",
        display: "(UTC-06:00) Guadalajara, Mexico City, Monterrey",
        iana: "America/Mexico_City",
    },
    WindowsZone {
        id: "Canada Central Standard Time",
        display: "(UTC-06:00) Saskatchewan",
        iana: "America/Regina",
    },
    WindowsZone {
        id: "Manitoba Standard Time",
        display: "(UTC-05:00) Manitoba",
        iana: "America/Winnipeg",
    },
    WindowsZone {
        id: "SA Pacific Standard Time",
        display: "(UTC-05:00) Bogota, Lima, Quito, Rio Branco",
        iana: "America/Bogota",
    },
    WindowsZone {
        id: "Eastern Standard Time (Mexico)",
        display: "(UTC-05:00) Chetumal",
        iana: "America/Cancun",
    },
    WindowsZone {
        id: "Eastern Standard Time",
        display: "(UTC-05:00) Eastern Time (US & Canada)",
        iana: "America/New_York",
    },
    WindowsZone {
        id: "Haiti Standard Time",
        display: "(UTC-05:00) Haiti",
        iana: "America/Port-au-Prince",
    },
    WindowsZone {
        id: "Cuba Standard Time",
        display: "(UTC-05:00) Havana",
        iana: "America/Havana",
    },
    WindowsZone {
        id: "US Eastern Standard Time",
        display: "(UTC-05:00) Indiana (East)",
        iana: "America/Indiana/Indianapolis",
    },
    WindowsZone {
        id: "Turks And Caicos Standard Time",
        display: "(UTC-05:00) Turks and Caicos",
        iana: "America/Grand_Turk",
    },
    WindowsZone {
        id: "Paraguay Standard Time",
        display: "(UTC-04:00) Asuncion",
        iana: "America/Asuncion",
    },
    WindowsZone {
        id: "Atlantic Standard Time",
        display: "(UTC-04:00) Atlantic Time (Canada)",
        iana: "America/Halifax",
    },
    WindowsZone {
        id: "Venezuela Standard Time",
        display: "(UTC-04:00) Caracas",
        iana: "America/Caracas",
    },
    WindowsZone {
        id: "Central Brazilian Standard Time",
        display: "(UTC-04:00) Cuiaba",
        iana: "America/Cuiaba",
    },
    WindowsZone {
        id: "SA Western Standard Time",
        display: "(UTC-04:00) Georgetown, La Paz, Manaus, San Juan",
        iana: "America/La_Paz",
    },
    WindowsZone {
        id: "Pacific SA Standard Time",
        display: "(UTC-04:00) Santiago",
        iana: "America/Santiago",
    },
    WindowsZone {
        id: "Newfoundland Standard Time",
        display: "(UTC-03:30) Newfoundland",
        iana: "America/St_Johns",
    },
    WindowsZone {
        id: "Tocantins Standard Time",
        display: "(UTC-03:00) Araguaina",
        iana: "America/Araguaina",
    },
    WindowsZone {
        id: "E. South America Standard Time",
        display: "(UTC-03:00) Brasilia",
        iana: "America/Sao_Paulo",
    },
    WindowsZone {
        id: "SA Eastern Standard Time",
        display: "(UTC-03:00) Cayenne, Fortaleza",
        iana: "America/Cayenne",
    },
    WindowsZone {
        id: "Argentina Standard Time",
        display: "(UTC-03:00) City of Buenos Aires",
        iana: "America/Argentina/Buenos_Aires",
    },
    WindowsZone {
        id: "Greenland Standard Time",
        display: "(UTC-03:00) Greenland",
        iana: "America/Nuuk",
    },
    WindowsZone {
        id: "Montevideo Standard Time",
        display: "(UTC-03:00) Montevideo",
        iana: "America/Montevideo",
    },
    WindowsZone {
        id: "Magallanes Standard Time",
        display: "(UTC-03:00) Punta Arenas",
        iana: "America/Punta_Arenas",
    },
    WindowsZone {
        id: "Saint Pierre Standard Time",
        display: "(UTC-03:00) Saint Pierre and Miquelon",
        iana: "America/Miquelon",
    },
    WindowsZone {
        id: "Bahia Standard Time",
        display: "(UTC-03:00) Salvador",
        iana: "America/Bahia",
    },
    WindowsZone {
        id: "UTC-02",
        display: "(UTC-02:00) Coordinated Universal Time-02",
        iana: "Etc/GMT+2",
    },
    WindowsZone {
        id: "Azores Standard Time",
        display: "(UTC-01:00) Azores",
        iana: "Atlantic/Azores",
    },
    WindowsZone {
        id: "Cape Verde Standard Time",
        display: "(UTC-01:00) Cabo Verde Is.",
        iana: "Atlantic/Cape_Verde",
    },
    WindowsZone {
        id: "UTC",
        display: "(UTC) Coordinated Universal Time",
        iana: "Etc/UTC",
    },
    WindowsZone {
        id: "GMT Standard Time",
        display: "(UTC+00:00) Dublin, Edinburgh, Lisbon, London",
        iana: "Europe/London",
    },
    WindowsZone {
        id: "Greenwich Standard Time",
        display: "(UTC+00:00) Monrovia, Reykjavik",
        iana: "Atlantic/Reykjavik",
    },
    WindowsZone {
        id: "Sao Tome Standard Time",
        display: "(UTC+00:00) Sao Tome",
        iana: "Africa/Sao_Tome",
    },
    WindowsZone {
        id: "Morocco Standard Time",
        display: "(UTC+01:00) Casablanca",
        iana: "Africa/Casablanca",
    },
    WindowsZone {
        id: "W. Europe Standard Time",
        display: "(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna",
        iana: "Europe/Berlin",
    },
    WindowsZone {
        id: "Central Europe Standard Time",
        display: "(UTC+01:00) Belgrade, Bratislava, Budapest, Ljubljana, Prague",
        iana: "Europe/Budapest",
    },
    WindowsZone {
        id: "Romance Standard Time",
        display: "(UTC+01:00) Brussels, Copenhagen, Madrid, Paris",
        iana: "Europe/Paris",
    },
    WindowsZone {
        id: "Central European Standard Time",
        display: "(UTC+01:00) Sarajevo, Skopje, Warsaw, Zagreb",
        iana: "Europe/Warsaw",
    },
    WindowsZone {
        id: "W. Central Africa Standard Time",
        display: "(UTC+01:00) West Central Africa",
        iana: "Africa/Lagos",
    },
    WindowsZone {
        id: "Jordan Standard Time",
        display: "(UTC+02:00) Amman",
        iana: "Asia/Amman",
    },
    WindowsZone {
        id: "GTB Standard Time",
        display: "(UTC+02:00) Athens, Bucharest",
        iana: "Europe/Bucharest",
    },
    WindowsZone {
        id: "Middle East Standard Time",
        display: "(UTC+02:00) Beirut",
        iana: "Asia/Beirut",
    },
    WindowsZone {
        id: "Egypt Standard Time",
        display: "(UTC+02:00) Cairo",
        iana: "Africa/Cairo",
    },
    WindowsZone {
        id: "E. Europe Standard Time",
        display: "(UTC+02:00) Chisinau",
        iana: "Europe/Chisinau",
    },
    WindowsZone {
        id: "Syria Standard Time",
        display: "(UTC+02:00) Damascus",
        iana: "Asia/Damascus",
    },
    WindowsZone {
        id: "West Bank Standard Time",
        display: "(UTC+02:00) Gaza, Hebron",
        iana: "Asia/Hebron",
    },
    WindowsZone {
        id: "South Africa Standard Time",
        display: "(UTC+02:00) Harare, Pretoria",
        iana: "Africa/Johannesburg",
    },
    WindowsZone {
        id: "FLE Standard Time",
        display: "(UTC+02:00) Helsinki, Kyiv, Riga, Sofia, Tallinn, Vilnius",
        iana: "Europe/Kyiv",
    },
    WindowsZone {
        id: "Israel Standard Time",
        display: "(UTC+02:00) Jerusalem",
        iana: "Asia/Jerusalem",
    },
    WindowsZone {
        id: "South Sudan Standard Time",
        display: "(UTC+02:00) Juba",
        iana: "Africa/Juba",
    },
    WindowsZone {
        id: "Kaliningrad Standard Time",
        display: "(UTC+02:00) Kaliningrad",
        iana: "Europe/Kaliningrad",
    },
    WindowsZone {
        id: "Sudan Standard Time",
        display: "(UTC+02:00) Khartoum",
        iana: "Africa/Khartoum",
    },
    WindowsZone {
        id: "Libya Standard Time",
        display: "(UTC+02:00) Tripoli",
        iana: "Africa/Tripoli",
    },
    WindowsZone {
        id: "Namibia Standard Time",
        display: "(UTC+02:00) Windhoek",
        iana: "Africa/Windhoek",
    },
    WindowsZone {
        id: "Arabic Standard Time",
        display: "(UTC+03:00) Baghdad",
        iana: "Asia/Baghdad",
    },
    WindowsZone {
        id: "Turkey Standard Time",
        display: "(UTC+03:00) Istanbul",
        iana: "Europe/Istanbul",
    },
    WindowsZone {
        id: "Arab Standard Time",
        display: "(UTC+03:00) Kuwait, Riyadh",
        iana: "Asia/Riyadh",
    },
    WindowsZone {
        id: "Belarus Standard Time",
        display: "(UTC+03:00) Minsk",
        iana: "Europe/Minsk",
    },
    WindowsZone {
        id: "Russian Standard Time",
        display: "(UTC+03:00) Moscow, St. Petersburg",
        iana: "Europe/Moscow",
    },
    WindowsZone {
        id: "E. Africa Standard Time",
        display: "(UTC+03:00) Nairobi",
        iana: "Africa/Nairobi",
    },
    WindowsZone {
        id: "Iran Standard Time",
        display: "(UTC+03:30) Tehran",
        iana: "Asia/Tehran",
    },
    WindowsZone {
        id: "Arabian Standard Time",
        display: "(UTC+04:00) Abu Dhabi, Muscat",
        iana: "Asia/Dubai",
    },
    WindowsZone {
        id: "Astrakhan Standard Time",
        display: "(UTC+04:00) Astrakhan, Ulyanovsk",
        iana: "Europe/Astrakhan",
    },
    WindowsZone {
        id: "Azerbaijan Standard Time",
        display: "(UTC+04:00) Baku",
        iana: "Asia/Baku",
    },
    WindowsZone {
        id: "Russia Time Zone 3",
        display: "(UTC+04:00) Izhevsk, Samara",
        iana: "Europe/Samara",
    },
    WindowsZone {
        id: "Mauritius Standard Time",
        display: "(UTC+04:00) Port Louis",
        iana: "Indian/Mauritius",
    },
    WindowsZone {
        id: "Saratov Standard Time",
        display: "(UTC+04:00) Saratov",
        iana: "Europe/Saratov",
    },
    WindowsZone {
        id: "Georgian Standard Time",
        display: "(UTC+04:00) Tbilisi",
        iana: "Asia/Tbilisi",
    },
    WindowsZone {
        id: "Volgograd Standard Time",
        display: "(UTC+04:00) Volgograd",
        iana: "Europe/Volgograd",
    },
    WindowsZone {
        id: "Caucasus Standard Time",
        display: "(UTC+04:00) Yerevan",
        iana: "Asia/Yerevan",
    },
    WindowsZone {
        id: "Afghanistan Standard Time",
        display: "(UTC+04:30) Kabul",
        iana: "Asia/Kabul",
    },
    WindowsZone {
        id: "West Asia Standard Time",
        display: "(UTC+05:00) Ashgabat, Tashkent",
        iana: "Asia/Tashkent",
    },
    WindowsZone {
        id: "Ekaterinburg Standard Time",
        display: "(UTC+05:00) Ekaterinburg",
        iana: "Asia/Yekaterinburg",
    },
    WindowsZone {
        id: "Pakistan Standard Time",
        display: "(UTC+05:00) Islamabad, Karachi",
        iana: "Asia/Karachi",
    },
    WindowsZone {
        id: "Qyzylorda Standard Time",
        display: "(UTC+05:00) Qyzylorda",
        iana: "Asia/Qyzylorda",
    },
    WindowsZone {
        id: "India Standard Time",
        display: "(UTC+05:30) Chennai, Kolkata, Mumbai, New Delhi",
        iana: "Asia/Kolkata",
    },
    WindowsZone {
        id: "Sri Lanka Standard Time",
        display: "(UTC+05:30) Sri Jayawardenepura",
        iana: "Asia/Colombo",
    },
    WindowsZone {
        id: "Nepal Standard Time",
        display: "(UTC+05:45) Kathmandu",
        iana: "Asia/Kathmandu",
    },
    WindowsZone {
        id: "Central Asia Standard Time",
        display: "(UTC+06:00) Astana",
        iana: "Asia/Bishkek",
    },
    WindowsZone {
        id: "Bangladesh Standard Time",
        display: "(UTC+06:00) Dhaka",
        iana: "Asia/Dhaka",
    },
    WindowsZone {
        id: "Omsk Standard Time",
        display: "(UTC+06:00) Omsk",
        iana: "Asia/Omsk",
    },
    WindowsZone {
        id: "Myanmar Standard Time",
        display: "(UTC+06:30) Yangon (Rangoon)",
        iana: "Asia/Yangon",
    },
    WindowsZone {
        id: "SE Asia Standard Time",
        display: "(UTC+07:00) Bangkok, Hanoi, Jakarta",
        iana: "Asia/Bangkok",
    },
    WindowsZone {
        id: "Altai Standard Time",
        display: "(UTC+07:00) Barnaul, Gorno-Altaysk",
        iana: "Asia/Barnaul",
    },
    WindowsZone {
        id: "W. Mongolia Standard Time",
        display: "(UTC+07:00) Hovd",
        iana: "Asia/Hovd",
    },
    WindowsZone {
        id: "North Asia Standard Time",
        display: "(UTC+07:00) Krasnoyarsk",
        iana: "Asia/Krasnoyarsk",
    },
    WindowsZone {
        id: "N. Central Asia Standard Time",
        display: "(UTC+07:00) Novosibirsk",
        iana: "Asia/Novosibirsk",
    },
    WindowsZone {
        id: "Tomsk Standard Time",
        display: "(UTC+07:00) Tomsk",
        iana: "Asia/Tomsk",
    },
    WindowsZone {
        id: "China Standard Time",
        display: "(UTC+08:00) Beijing, Chongqing, Hong Kong, Urumqi",
        iana: "Asia/Shanghai",
    },
    WindowsZone {
        id: "North Asia East Standard Time",
        display: "(UTC+08:00) Irkutsk",
        iana: "Asia/Irkutsk",
    },
    WindowsZone {
        id: "Singapore Standard Time",
        display: "(UTC+08:00) Kuala Lumpur, Singapore",
        iana: "Asia/Singapore",
    },
    WindowsZone {
        id: "W. Australia Standard Time",
        display: "(UTC+08:00) Perth",
        iana: "Australia/Perth",
    },
    WindowsZone {
        id: "Taipei Standard Time",
        display: "(UTC+08:00) Taipei",
        iana: "Asia/Taipei",
    },
    WindowsZone {
        id: "Ulaanbaatar Standard Time",
        display: "(UTC+08:00) Ulaanbaatar",
        iana: "Asia/Ulaanbaatar",
    },
    WindowsZone {
        id: "Aus Central W. Standard Time",
        display: "(UTC+08:45) Eucla",
        iana: "Australia/Eucla",
    },
    WindowsZone {
        id: "Transbaikal Standard Time",
        display: "(UTC+09:00) Chita",
        iana: "Asia/Chita",
    },
    WindowsZone {
        id: "Tokyo Standard Time",
        display: "(UTC+09:00) Osaka, Sapporo, Tokyo",
        iana: "Asia/Tokyo",
    },
    WindowsZone {
        id: "North Korea Standard Time",
        display: "(UTC+09:00) Pyongyang",
        iana: "Asia/Pyongyang",
    },
    WindowsZone {
        id: "Korea Standard Time",
        display: "(UTC+09:00) Seoul",
        iana: "Asia/Seoul",
    },
    WindowsZone {
        id: "Yakutsk Standard Time",
        display: "(UTC+09:00) Yakutsk",
        iana: "Asia/Yakutsk",
    },
    WindowsZone {
        id: "Cen. Australia Standard Time",
        display: "(UTC+09:30) Adelaide",
        iana: "Australia/Adelaide",
    },
    WindowsZone {
        id: "AUS Central Standard Time",
        display: "(UTC+09:30) Darwin",
        iana: "Australia/Darwin",
    },
    WindowsZone {
        id: "E. Australia Standard Time",
        display: "(UTC+10:00) Brisbane",
        iana: "Australia/Brisbane",
    },
    WindowsZone {
        id: "AUS Eastern Standard Time",
        display: "(UTC+10:00) Canberra, Melbourne, Sydney",
        iana: "Australia/Sydney",
    },
    WindowsZone {
        id: "West Pacific Standard Time",
        display: "(UTC+10:00) Guam, Port Moresby",
        iana: "Pacific/Port_Moresby",
    },
    WindowsZone {
        id: "Tasmania Standard Time",
        display: "(UTC+10:00) Hobart",
        iana: "Australia/Hobart",
    },
    WindowsZone {
        id: "Vladivostok Standard Time",
        display: "(UTC+10:00) Vladivostok",
        iana: "Asia/Vladivostok",
    },
    WindowsZone {
        id: "Lord Howe Standard Time",
        display: "(UTC+10:30) Lord Howe Island",
        iana: "Australia/Lord_Howe",
    },
    WindowsZone {
        id: "Bougainville Standard Time",
        display: "(UTC+11:00) Bougainville Island",
        iana: "Pacific/Bougainville",
    },
    WindowsZone {
        id: "Russia Time Zone 10",
        display: "(UTC+11:00) Chokurdakh",
        iana: "Asia/Srednekolymsk",
    },
    WindowsZone {
        id: "Magadan Standard Time",
        display: "(UTC+11:00) Magadan",
        iana: "Asia/Magadan",
    },
    WindowsZone {
        id: "Norfolk Standard Time",
        display: "(UTC+11:00) Norfolk Island",
        iana: "Pacific/Norfolk",
    },
    WindowsZone {
        id: "Sakhalin Standard Time",
        display: "(UTC+11:00) Sakhalin",
        iana: "Asia/Sakhalin",
    },
    WindowsZone {
        id: "Central Pacific Standard Time",
        display: "(UTC+11:00) Solomon Is., New Caledonia",
        iana: "Pacific/Guadalcanal",
    },
    WindowsZone {
        id: "Russia Time Zone 11",
        display: "(UTC+12:00) Anadyr, Petropavlovsk-Kamchatsky",
        iana: "Asia/Kamchatka",
    },
    WindowsZone {
        id: "New Zealand Standard Time",
        display: "(UTC+12:00) Auckland, Wellington",
        iana: "Pacific/Auckland",
    },
    WindowsZone {
        id: "UTC+12",
        display: "(UTC+12:00) Coordinated Universal Time+12",
        iana: "Etc/GMT-12",
    },
    WindowsZone {
        id: "Fiji Standard Time",
        display: "(UTC+12:00) Fiji",
        iana: "Pacific/Fiji",
    },
    WindowsZone {
        id: "Chatham Islands Standard Time",
        display: "(UTC+12:45) Chatham Islands",
        iana: "Pacific/Chatham",
    },
    WindowsZone {
        id: "UTC+13",
        display: "(UTC+13:00) Coordinated Universal Time+13",
        iana: "Etc/GMT-13",
    },
    WindowsZone {
        id: "Tonga Standard Time",
        display: "(UTC+13:00) Nuku'alofa",
        iana: "Pacific/Tongatapu",
    },
    WindowsZone {
        id: "Samoa Standard Time",
        display: "(UTC+13:00) Samoa",
        iana: "Pacific/Apia",
    },
    WindowsZone {
        id: "Line Islands Standard Time",
        display: "(UTC+14:00) Kiritimati Island",
        iana: "Pacific/Kiritimati",
    },
];

/// Windows zone ids that were removed from Windows/CLDR but still appear in
/// old Exchange and Outlook data, with their IANA equivalent.
pub static LEGACY_WINDOWS_ZONES: &[(&str, &str)] = &[
    ("Mid-Atlantic Standard Time", "Etc/GMT+2"),
    ("Kamchatka Standard Time", "Asia/Kamchatka"),
    ("Mexico Standard Time", "America/Mexico_City"),
    ("Mexico Standard Time 2", "America/Chihuahua"),
    ("Armenian Standard Time", "Asia/Yerevan"),
    ("Jerusalem Standard Time", "Asia/Jerusalem"),
    ("Coordinated Universal Time", "Etc/UTC"),
    ("UTC-02", "Etc/GMT+2"),
    ("UTC+00", "Etc/UTC"),
];

/// Display names used by older Windows versions (TZIDs written by Outlook
/// 2007–2013 and Exchange 2003–2010), mapped to IANA zones. Display names
/// are matched after normalization (`GMT`→`UTC`, `.`→`:` in the offset,
/// `/`→`,` between cities, case-insensitive).
pub static LEGACY_DISPLAY_NAMES: &[(&str, &str)] = &[
    ("(UTC) Dublin, Edinburgh, Lisbon, London", "Europe/London"),
    ("(UTC) Monrovia, Reykjavik", "Atlantic/Reykjavik"),
    ("(UTC) Casablanca", "Africa/Casablanca"),
    ("(UTC+00:00) Casablanca", "Africa/Casablanca"),
    (
        "(UTC) Greenwich Mean Time : Dublin, Edinburgh, Lisbon, London",
        "Europe/London",
    ),
    (
        "(UTC+02:00) Athens, Bucharest, Istanbul",
        "Europe/Bucharest",
    ),
    (
        "(UTC+02:00) Athens, Beirut, Istanbul, Minsk",
        "Europe/Athens",
    ),
    ("(UTC+02:00) Istanbul", "Europe/Istanbul"),
    (
        "(UTC+02:00) Helsinki, Kiev, Riga, Sofia, Tallinn, Vilnius",
        "Europe/Kyiv",
    ),
    (
        "(UTC+02:00) Helsinki, Kyiv, Riga, Sofia, Tallinn, Vilnius",
        "Europe/Kyiv",
    ),
    ("(UTC+02:00) Minsk", "Europe/Minsk"),
    ("(UTC+02:00) E. Europe", "Europe/Chisinau"),
    ("(UTC+02:00) Bucharest", "Europe/Bucharest"),
    ("(UTC+02:00) Athens, Bucharest", "Europe/Bucharest"),
    (
        "(UTC+03:00) Moscow, St. Petersburg, Volgograd",
        "Europe/Moscow",
    ),
    (
        "(UTC+04:00) Moscow, St. Petersburg, Volgograd",
        "Europe/Moscow",
    ),
    (
        "(UTC+03:00) Moscow, St. Petersburg, Volgograd (RTZ 2)",
        "Europe/Moscow",
    ),
    ("(UTC+03:00) Kaliningrad, Minsk", "Europe/Kaliningrad"),
    ("(UTC+03:00) Amman", "Asia/Amman"),
    ("(UTC+03:00) Damascus", "Asia/Damascus"),
    ("(UTC-03:00) Buenos Aires", "America/Argentina/Buenos_Aires"),
    ("(UTC-03:00) Brasilia", "America/Sao_Paulo"),
    ("(UTC-04:00) Santiago", "America/Santiago"),
    ("(UTC-04:30) Caracas", "America/Caracas"),
    ("(UTC-04:00) Georgetown, La Paz, San Juan", "America/La_Paz"),
    ("(UTC-04:00) Manaus", "America/Manaus"),
    ("(UTC-05:00) Bogota, Lima, Quito", "America/Bogota"),
    (
        "(UTC-06:00) Guadalajara, Mexico City, Monterrey - Old",
        "America/Mexico_City",
    ),
    (
        "(UTC-06:00) Guadalajara, Mexico City, Monterrey - New",
        "America/Mexico_City",
    ),
    (
        "(UTC-07:00) Chihuahua, La Paz, Mazatlan - Old",
        "America/Chihuahua",
    ),
    (
        "(UTC-07:00) Chihuahua, La Paz, Mazatlan - New",
        "America/Mazatlan",
    ),
    ("(UTC-08:00) Tijuana, Baja California", "America/Tijuana"),
    ("(UTC-02:00) Mid-Atlantic", "Etc/GMT+2"),
    ("(UTC-02:00) Mid-Atlantic - Old", "Etc/GMT+2"),
    ("(UTC+04:00) Abu Dhabi, Muscat", "Asia/Dubai"),
    ("(UTC+04:00) Caucasus Standard Time", "Asia/Yerevan"),
    ("(UTC+05:00) Tashkent", "Asia/Tashkent"),
    ("(UTC+05:00) Ekaterinburg", "Asia/Yekaterinburg"),
    ("(UTC+06:00) Ekaterinburg", "Asia/Yekaterinburg"),
    ("(UTC+06:00) Almaty, Novosibirsk", "Asia/Almaty"),
    ("(UTC+06:00) Novosibirsk", "Asia/Novosibirsk"),
    ("(UTC+07:00) Novosibirsk", "Asia/Novosibirsk"),
    ("(UTC+08:00) Krasnoyarsk", "Asia/Krasnoyarsk"),
    ("(UTC+09:00) Irkutsk", "Asia/Irkutsk"),
    ("(UTC+10:00) Yakutsk", "Asia/Yakutsk"),
    ("(UTC+11:00) Vladivostok", "Asia/Vladivostok"),
    (
        "(UTC+11:00) Magadan, Solomon Is., New Caledonia",
        "Asia/Magadan",
    ),
    ("(UTC+12:00) Magadan", "Asia/Magadan"),
    ("(UTC+12:00) Fiji, Kamchatka, Marshall Is.", "Pacific/Fiji"),
    ("(UTC+12:00) Fiji", "Pacific/Fiji"),
    (
        "(UTC+12:00) Petropavlovsk-Kamchatsky - Old",
        "Asia/Kamchatka",
    ),
    ("(UTC+13:00) Samoa", "Pacific/Apia"),
    ("(UTC+08:00) Ulaanbaatar", "Asia/Ulaanbaatar"),
    (
        "(UTC+08:00) Beijing, Chongqing, Hong Kong, Urumqi",
        "Asia/Shanghai",
    ),
    ("(UTC+09:30) Adelaide", "Australia/Adelaide"),
    (
        "(UTC+10:00) Canberra, Melbourne, Sydney",
        "Australia/Sydney",
    ),
    ("(UTC-05:00) Eastern Time (US & Canada)", "America/New_York"),
    ("(UTC-06:00) Central Time (US & Canada)", "America/Chicago"),
    ("(UTC-07:00) Mountain Time (US & Canada)", "America/Denver"),
    (
        "(UTC-08:00) Pacific Time (US & Canada)",
        "America/Los_Angeles",
    ),
];
