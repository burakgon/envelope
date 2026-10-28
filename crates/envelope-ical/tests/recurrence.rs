//! Recurrence expansion: RFC 5545 examples, a differential test against an
//! independent implementation, and real-world override / DST scenarios.

mod common;

use chrono::{DateTime, Datelike, Duration, NaiveDateTime, Utc};
use chrono_tz::Tz;
use common::{dt, fmt, text, utc};
use envelope_ical::tz::wall_clock;
use envelope_ical::{
    Event, EventStatus, EventTime, ExpandOptions, TzContext, expand, expand_events, parse_calendar,
};

const NY: &str = "America/New_York";

fn ny_series(start: &str, rrule: &str) -> Event {
    let start = dt(start);
    let mut event = Event::new(
        "rfc",
        EventTime::Zoned {
            local: start,
            tzid: NY.into(),
        },
        EventTime::Zoned {
            local: start + Duration::hours(1),
            tzid: NY.into(),
        },
    );
    event.rrule = Some(rrule.into());
    event
}

/// Local wall-clock starts ("1997-09-02 09:00") of all occurrences in the window.
fn locals(event: &Event, from: &str, to: &str) -> Vec<String> {
    let ctx = TzContext::new();
    let expansion = expand(
        event,
        &[],
        utc(from),
        utc(to),
        &ctx,
        &ExpandOptions::default(),
    );
    assert!(!expansion.truncated);
    expansion
        .occurrences
        .iter()
        .map(|o| {
            wall_clock(&o.start_time)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .collect()
}

fn days(year_month_days: &[(i32, u32, &[u32])], time: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (year, month, list) in year_month_days {
        for day in *list {
            out.push(format!("{year:04}-{month:02}-{day:02} {time}"));
        }
    }
    out
}

const FOREVER: (&str, &str) = ("1990-01-01 00:00", "2010-01-01 00:00");

#[test]
fn rfc5545_daily_examples() {
    // Daily for 10 occurrences.
    let e = ny_series("1997-09-02 09:00", "FREQ=DAILY;COUNT=10");
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        days(&[(1997, 9, &[2, 3, 4, 5, 6, 7, 8, 9, 10, 11])], "09:00")
    );

    // Daily until December 24, 1997 — crosses the end of DST.
    let e = ny_series("1997-09-02 09:00", "FREQ=DAILY;UNTIL=19971224T000000Z");
    let all = locals(&e, FOREVER.0, FOREVER.1);
    assert_eq!(all.len(), 113);
    assert_eq!(all.first().map(String::as_str), Some("1997-09-02 09:00"));
    assert_eq!(all.last().map(String::as_str), Some("1997-12-23 09:00"));
    let ctx = TzContext::new();
    let occurrences = expand(
        &e,
        &[],
        utc("1997-10-25 00:00"),
        utc("1997-10-28 00:00"),
        &ctx,
        &ExpandOptions::default(),
    );
    let starts: Vec<String> = occurrences
        .occurrences
        .iter()
        .map(|o| fmt(&o.start))
        .collect();
    assert_eq!(
        starts,
        ["1997-10-25 13:00", "1997-10-26 14:00", "1997-10-27 14:00"]
    );

    // Every other day, forever.
    let e = ny_series("1997-09-02 09:00", "FREQ=DAILY;INTERVAL=2");
    assert_eq!(
        locals(&e, "1997-09-01 00:00", "1997-09-12 00:00"),
        days(&[(1997, 9, &[2, 4, 6, 8, 10])], "09:00")
    );

    // Every 10 days, 5 occurrences.
    let e = ny_series("1997-09-02 09:00", "FREQ=DAILY;INTERVAL=10;COUNT=5");
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        days(&[(1997, 9, &[2, 12, 22]), (1997, 10, &[2, 12])], "09:00")
    );

    // Every day in January, for 3 years (two equivalent rules).
    for rule in [
        "FREQ=YEARLY;UNTIL=20000131T140000Z;BYMONTH=1;BYDAY=SU,MO,TU,WE,TH,FR,SA",
        "FREQ=DAILY;UNTIL=20000131T140000Z;BYMONTH=1",
    ] {
        let e = ny_series("1998-01-01 09:00", rule);
        let all = locals(&e, FOREVER.0, FOREVER.1);
        assert_eq!(all.len(), 93, "{rule}");
        assert!(all.iter().all(|d| d[5..7] == *"01"), "{rule}");
        assert_eq!(all.last().map(String::as_str), Some("2000-01-31 09:00"));
    }
}

#[test]
fn rfc5545_weekly_examples() {
    let e = ny_series("1997-09-02 09:00", "FREQ=WEEKLY;COUNT=10");
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        days(
            &[
                (1997, 9, &[2, 9, 16, 23, 30]),
                (1997, 10, &[7, 14, 21, 28]),
                (1997, 11, &[4])
            ],
            "09:00"
        )
    );

    let e = ny_series("1997-09-02 09:00", "FREQ=WEEKLY;UNTIL=19971224T000000Z");
    assert_eq!(locals(&e, FOREVER.0, FOREVER.1).len(), 17);

    let e = ny_series("1997-09-02 09:00", "FREQ=WEEKLY;INTERVAL=2;WKST=SU");
    assert_eq!(
        locals(&e, "1997-09-01 00:00", "1998-02-20 00:00"),
        days(
            &[
                (1997, 9, &[2, 16, 30]),
                (1997, 10, &[14, 28]),
                (1997, 11, &[11, 25]),
                (1997, 12, &[9, 23]),
                (1998, 1, &[6, 20]),
                (1998, 2, &[3, 17])
            ],
            "09:00"
        )
    );

    let expected = days(
        &[
            (1997, 9, &[2, 4, 9, 11, 16, 18, 23, 25, 30]),
            (1997, 10, &[2]),
        ],
        "09:00",
    );
    for rule in [
        "FREQ=WEEKLY;UNTIL=19971007T000000Z;WKST=SU;BYDAY=TU,TH",
        "FREQ=WEEKLY;COUNT=10;WKST=SU;BYDAY=TU,TH",
    ] {
        let e = ny_series("1997-09-02 09:00", rule);
        assert_eq!(locals(&e, FOREVER.0, FOREVER.1), expected, "{rule}");
    }

    let e = ny_series(
        "1997-09-01 09:00",
        "FREQ=WEEKLY;INTERVAL=2;UNTIL=19971224T000000Z;WKST=SU;BYDAY=MO,WE,FR",
    );
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        days(
            &[
                (1997, 9, &[1, 3, 5, 15, 17, 19, 29]),
                (1997, 10, &[1, 3, 13, 15, 17, 27, 29, 31]),
                (1997, 11, &[10, 12, 14, 24, 26, 28]),
                (1997, 12, &[8, 10, 12, 22]),
            ],
            "09:00"
        )
    );

    let e = ny_series(
        "1997-09-02 09:00",
        "FREQ=WEEKLY;INTERVAL=2;COUNT=8;WKST=SU;BYDAY=TU,TH",
    );
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        days(
            &[(1997, 9, &[2, 4, 16, 18, 30]), (1997, 10, &[2, 14, 16])],
            "09:00"
        )
    );

    // WKST changes the result of an every-other-week rule.
    let e = ny_series(
        "1997-08-05 09:00",
        "FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=MO",
    );
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        days(&[(1997, 8, &[5, 10, 19, 24])], "09:00")
    );
    let e = ny_series(
        "1997-08-05 09:00",
        "FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=SU",
    );
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        days(&[(1997, 8, &[5, 17, 19, 31])], "09:00")
    );
}

#[test]
fn rfc5545_monthly_examples() {
    let e = ny_series("1997-09-05 09:00", "FREQ=MONTHLY;COUNT=10;BYDAY=1FR");
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        days(
            &[
                (1997, 9, &[5]),
                (1997, 10, &[3]),
                (1997, 11, &[7]),
                (1997, 12, &[5]),
                (1998, 1, &[2]),
                (1998, 2, &[6]),
                (1998, 3, &[6]),
                (1998, 4, &[3]),
                (1998, 5, &[1]),
                (1998, 6, &[5])
            ],
            "09:00"
        )
    );

    let e = ny_series(
        "1997-09-05 09:00",
        "FREQ=MONTHLY;UNTIL=19971224T000000Z;BYDAY=1FR",
    );
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        days(
            &[
                (1997, 9, &[5]),
                (1997, 10, &[3]),
                (1997, 11, &[7]),
                (1997, 12, &[5])
            ],
            "09:00"
        )
    );

    let e = ny_series(
        "1997-09-07 09:00",
        "FREQ=MONTHLY;INTERVAL=2;COUNT=10;BYDAY=1SU,-1SU",
    );
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        days(
            &[
                (1997, 9, &[7, 28]),
                (1997, 11, &[2, 30]),
                (1998, 1, &[4, 25]),
                (1998, 3, &[1, 29]),
                (1998, 5, &[3, 31])
            ],
            "09:00"
        )
    );

    let e = ny_series("1997-09-22 09:00", "FREQ=MONTHLY;COUNT=6;BYDAY=-2MO");
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        days(
            &[
                (1997, 9, &[22]),
                (1997, 10, &[20]),
                (1997, 11, &[17]),
                (1997, 12, &[22]),
                (1998, 1, &[19]),
                (1998, 2, &[16])
            ],
            "09:00"
        )
    );

    let e = ny_series("1997-09-28 09:00", "FREQ=MONTHLY;BYMONTHDAY=-3");
    assert_eq!(
        locals(&e, "1997-09-01 00:00", "1998-03-01 00:00"),
        days(
            &[
                (1997, 9, &[28]),
                (1997, 10, &[29]),
                (1997, 11, &[28]),
                (1997, 12, &[29]),
                (1998, 1, &[29]),
                (1998, 2, &[26])
            ],
            "09:00"
        )
    );

    let e = ny_series("1997-09-02 09:00", "FREQ=MONTHLY;COUNT=10;BYMONTHDAY=2,15");
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        days(
            &[
                (1997, 9, &[2, 15]),
                (1997, 10, &[2, 15]),
                (1997, 11, &[2, 15]),
                (1997, 12, &[2, 15]),
                (1998, 1, &[2, 15])
            ],
            "09:00"
        )
    );

    let e = ny_series("1997-09-30 09:00", "FREQ=MONTHLY;COUNT=10;BYMONTHDAY=1,-1");
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        days(
            &[
                (1997, 9, &[30]),
                (1997, 10, &[1, 31]),
                (1997, 11, &[1, 30]),
                (1997, 12, &[1, 31]),
                (1998, 1, &[1, 31]),
                (1998, 2, &[1])
            ],
            "09:00"
        )
    );

    let e = ny_series(
        "1997-09-10 09:00",
        "FREQ=MONTHLY;INTERVAL=18;COUNT=10;BYMONTHDAY=10,11,12,13,14,15",
    );
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        days(
            &[
                (1997, 9, &[10, 11, 12, 13, 14, 15]),
                (1999, 3, &[10, 11, 12, 13])
            ],
            "09:00"
        )
    );

    let e = ny_series("1997-09-02 09:00", "FREQ=MONTHLY;INTERVAL=2;BYDAY=TU");
    assert_eq!(
        locals(&e, "1997-09-01 00:00", "1998-04-01 00:00"),
        days(
            &[
                (1997, 9, &[2, 9, 16, 23, 30]),
                (1997, 11, &[4, 11, 18, 25]),
                (1998, 1, &[6, 13, 20, 27]),
                (1998, 3, &[3, 10, 17, 24, 31])
            ],
            "09:00"
        )
    );

    // Every Friday the 13th (DTSTART itself is excluded).
    let mut e = ny_series("1997-09-02 09:00", "FREQ=MONTHLY;BYDAY=FR;BYMONTHDAY=13");
    e.exdates.push(EventTime::Zoned {
        local: dt("1997-09-02 09:00"),
        tzid: NY.into(),
    });
    assert_eq!(
        locals(&e, "1997-01-01 00:00", "2000-12-31 00:00"),
        [
            "1998-02-13 09:00",
            "1998-03-13 09:00",
            "1998-11-13 09:00",
            "1999-08-13 09:00",
            "2000-10-13 09:00"
        ]
    );

    // The first Saturday that follows the first Sunday of the month.
    let e = ny_series(
        "1997-09-13 09:00",
        "FREQ=MONTHLY;BYDAY=SA;BYMONTHDAY=7,8,9,10,11,12,13",
    );
    assert_eq!(
        locals(&e, "1997-09-01 00:00", "1998-06-30 00:00"),
        days(
            &[
                (1997, 9, &[13]),
                (1997, 10, &[11]),
                (1997, 11, &[8]),
                (1997, 12, &[13]),
                (1998, 1, &[10]),
                (1998, 2, &[7]),
                (1998, 3, &[7]),
                (1998, 4, &[11]),
                (1998, 5, &[9]),
                (1998, 6, &[13])
            ],
            "09:00"
        )
    );

    // BYSETPOS: third of Tue/Wed/Thu, and the second-to-last weekday.
    let e = ny_series(
        "1997-09-04 09:00",
        "FREQ=MONTHLY;COUNT=3;BYDAY=TU,WE,TH;BYSETPOS=3",
    );
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        days(
            &[(1997, 9, &[4]), (1997, 10, &[7]), (1997, 11, &[6])],
            "09:00"
        )
    );
    let e = ny_series(
        "1997-09-29 09:00",
        "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-2",
    );
    assert_eq!(
        locals(&e, "1997-09-01 00:00", "1998-04-01 00:00"),
        days(
            &[
                (1997, 9, &[29]),
                (1997, 10, &[30]),
                (1997, 11, &[27]),
                (1997, 12, &[30]),
                (1998, 1, &[29]),
                (1998, 2, &[26]),
                (1998, 3, &[30])
            ],
            "09:00"
        )
    );

    // Invalid dates (February 30) are skipped, not clamped.
    let e = ny_series("2007-01-15 09:00", "FREQ=MONTHLY;BYMONTHDAY=15,30;COUNT=5");
    assert_eq!(
        locals(&e, "2007-01-01 00:00", "2008-01-01 00:00"),
        days(
            &[(2007, 1, &[15, 30]), (2007, 2, &[15]), (2007, 3, &[15, 30])],
            "09:00"
        )
    );
}

#[test]
fn rfc5545_yearly_examples() {
    let e = ny_series("1997-06-10 09:00", "FREQ=YEARLY;COUNT=10;BYMONTH=6,7");
    let expected: Vec<String> = (1997..=2001)
        .flat_map(|y| [format!("{y}-06-10 09:00"), format!("{y}-07-10 09:00")])
        .collect();
    assert_eq!(locals(&e, FOREVER.0, FOREVER.1), expected);

    let e = ny_series(
        "1997-03-10 09:00",
        "FREQ=YEARLY;INTERVAL=2;COUNT=10;BYMONTH=1,2,3",
    );
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        days(
            &[
                (1997, 3, &[10]),
                (1999, 1, &[10]),
                (1999, 2, &[10]),
                (1999, 3, &[10]),
                (2001, 1, &[10]),
                (2001, 2, &[10]),
                (2001, 3, &[10]),
                (2003, 1, &[10]),
                (2003, 2, &[10]),
                (2003, 3, &[10])
            ],
            "09:00"
        )
    );

    let e = ny_series(
        "1997-01-01 09:00",
        "FREQ=YEARLY;INTERVAL=3;COUNT=10;BYYEARDAY=1,100,200",
    );
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        [
            "1997-01-01 09:00",
            "1997-04-10 09:00",
            "1997-07-19 09:00",
            "2000-01-01 09:00",
            "2000-04-09 09:00",
            "2000-07-18 09:00",
            "2003-01-01 09:00",
            "2003-04-10 09:00",
            "2003-07-19 09:00",
            "2006-01-01 09:00",
        ]
    );

    let e = ny_series("1997-05-19 09:00", "FREQ=YEARLY;BYDAY=20MO");
    assert_eq!(
        locals(&e, "1997-01-01 00:00", "2000-01-01 00:00"),
        ["1997-05-19 09:00", "1998-05-18 09:00", "1999-05-17 09:00"]
    );

    let e = ny_series("1997-05-12 09:00", "FREQ=YEARLY;BYWEEKNO=20;BYDAY=MO");
    assert_eq!(
        locals(&e, "1997-01-01 00:00", "2000-01-01 00:00"),
        ["1997-05-12 09:00", "1998-05-11 09:00", "1999-05-17 09:00"]
    );

    let e = ny_series("1997-03-13 09:00", "FREQ=YEARLY;BYMONTH=3;BYDAY=TH");
    assert_eq!(
        locals(&e, "1997-01-01 00:00", "2000-01-01 00:00"),
        days(
            &[
                (1997, 3, &[13, 20, 27]),
                (1998, 3, &[5, 12, 19, 26]),
                (1999, 3, &[4, 11, 18, 25])
            ],
            "09:00"
        )
    );

    let e = ny_series("1997-06-05 09:00", "FREQ=YEARLY;BYDAY=TH;BYMONTH=6,7,8");
    assert_eq!(
        locals(&e, "1997-01-01 00:00", "2000-01-01 00:00"),
        days(
            &[
                (1997, 6, &[5, 12, 19, 26]),
                (1997, 7, &[3, 10, 17, 24, 31]),
                (1997, 8, &[7, 14, 21, 28]),
                (1998, 6, &[4, 11, 18, 25]),
                (1998, 7, &[2, 9, 16, 23, 30]),
                (1998, 8, &[6, 13, 20, 27]),
                (1999, 6, &[3, 10, 17, 24]),
                (1999, 7, &[1, 8, 15, 22, 29]),
                (1999, 8, &[5, 12, 19, 26]),
            ],
            "09:00"
        )
    );

    // US presidential election day.
    let e = ny_series(
        "1996-11-05 09:00",
        "FREQ=YEARLY;INTERVAL=4;BYMONTH=11;BYDAY=TU;BYMONTHDAY=2,3,4,5,6,7,8",
    );
    assert_eq!(
        locals(&e, "1996-01-01 00:00", "2005-01-01 00:00"),
        ["1996-11-05 09:00", "2000-11-07 09:00", "2004-11-02 09:00"]
    );
}

#[test]
fn rfc5545_sub_daily_examples() {
    // Every 3 hours from 9:00 to 17:00 (floating, as the RFC's UTC UNTIL is inconsistent).
    let mut e = ny_series(
        "1997-09-02 09:00",
        "FREQ=HOURLY;INTERVAL=3;UNTIL=19970902T170000",
    );
    e.start = EventTime::Floating {
        local: dt("1997-09-02 09:00"),
    };
    e.end = EventTime::Floating {
        local: dt("1997-09-02 10:00"),
    };
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        ["1997-09-02 09:00", "1997-09-02 12:00", "1997-09-02 15:00"]
    );

    let e = ny_series("1997-09-02 09:00", "FREQ=MINUTELY;INTERVAL=15;COUNT=6");
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        [
            "1997-09-02 09:00",
            "1997-09-02 09:15",
            "1997-09-02 09:30",
            "1997-09-02 09:45",
            "1997-09-02 10:00",
            "1997-09-02 10:15"
        ]
    );

    let e = ny_series("1997-09-02 09:00", "FREQ=MINUTELY;INTERVAL=90;COUNT=4");
    assert_eq!(
        locals(&e, FOREVER.0, FOREVER.1),
        [
            "1997-09-02 09:00",
            "1997-09-02 10:30",
            "1997-09-02 12:00",
            "1997-09-02 13:30"
        ]
    );

    let mut expected = Vec::new();
    for day in [2, 3] {
        for hour in 9..=16 {
            for minute in [0, 20, 40] {
                expected.push(format!("1997-09-{day:02} {hour:02}:{minute:02}"));
            }
        }
    }
    for rule in [
        "FREQ=DAILY;BYHOUR=9,10,11,12,13,14,15,16;BYMINUTE=0,20,40",
        "FREQ=MINUTELY;INTERVAL=20;BYHOUR=9,10,11,12,13,14,15,16",
    ] {
        let e = ny_series("1997-09-02 09:00", rule);
        assert_eq!(
            locals(&e, "1997-09-02 00:00", "1997-09-04 04:00"),
            expected,
            "{rule}"
        );
    }
}

/// A tiny deterministic generator (no extra dependencies).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

fn random_rule(rng: &mut Lcg) -> String {
    const WEEKDAYS: [&str; 7] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];
    let freq = *rng.pick(&["YEARLY", "MONTHLY", "WEEKLY", "DAILY"]);
    let mut parts = vec![format!("FREQ={freq}")];
    if rng.chance(40) {
        parts.push(format!("INTERVAL={}", 1 + rng.below(4)));
    }
    let mut has_day_filter = false;
    if rng.chance(35) {
        let mut months: Vec<u64> = (0..1 + rng.below(3)).map(|_| 1 + rng.below(12)).collect();
        months.sort_unstable();
        months.dedup();
        parts.push(format!(
            "BYMONTH={}",
            months
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    if freq != "WEEKLY" && rng.chance(35) {
        let values: Vec<String> = (0..1 + rng.below(3))
            .map(|_| {
                let day = 1 + rng.below(31) as i64;
                if rng.chance(30) {
                    (-day).to_string()
                } else {
                    day.to_string()
                }
            })
            .collect();
        parts.push(format!("BYMONTHDAY={}", values.join(",")));
        has_day_filter = true;
    }
    if rng.chance(50) {
        let ordinals = matches!(freq, "MONTHLY" | "YEARLY") && rng.chance(40);
        let values: Vec<String> = (0..1 + rng.below(3))
            .map(|_| {
                let day = *rng.pick(&WEEKDAYS);
                if ordinals {
                    let n = 1 + rng.below(if freq == "MONTHLY" { 4 } else { 20 }) as i64;
                    let n = if rng.chance(30) { -n } else { n };
                    format!("{n}{day}")
                } else {
                    day.to_owned()
                }
            })
            .collect();
        parts.push(format!("BYDAY={}", values.join(",")));
        has_day_filter = true;
    }
    if freq == "YEARLY" && rng.chance(15) {
        let n = 1 + rng.below(53) as i64;
        parts.push(format!("BYWEEKNO={}", if rng.chance(20) { -n } else { n }));
    }
    if freq == "YEARLY" && rng.chance(15) {
        let n = 1 + rng.below(366) as i64;
        parts.push(format!("BYYEARDAY={}", if rng.chance(25) { -n } else { n }));
    }
    if rng.chance(15) {
        let hours: Vec<String> = (0..1 + rng.below(2))
            .map(|_| rng.below(24).to_string())
            .collect();
        parts.push(format!("BYHOUR={}", hours.join(",")));
    }
    if rng.chance(10) {
        parts.push(format!("BYMINUTE={}", rng.below(60)));
    }
    if has_day_filter && rng.chance(25) {
        let n = 1 + rng.below(3) as i64;
        parts.push(format!("BYSETPOS={}", if rng.chance(50) { -n } else { n }));
    }
    if rng.chance(30) {
        parts.push(format!("WKST={}", rng.pick(&WEEKDAYS)));
    }
    parts.join(";")
}

/// Compares the crate's engine with the independent `rrule` crate on
/// thousands of random rules (anchored in UTC, so only rule semantics are
/// compared). DTSTART is always part of an RFC 5545 recurrence set but not
/// of the oracle's, so it is added to the oracle's result.
#[test]
fn differential_against_independent_implementation() {
    let mut rng = Lcg(0x5eed_1234_abcd_0001);
    let mut compared = 0;
    for _ in 0..3_000 {
        let rule = random_rule(&mut rng);
        let start = utc("2000-01-01 00:00")
            + Duration::days(rng.below(365 * 30) as i64)
            + Duration::minutes(rng.below(24 * 60) as i64);
        let window_end = start + Duration::days(365 * 6);
        let oracle_text = format!("DTSTART:{}\nRRULE:{rule}", start.format("%Y%m%dT%H%M%SZ"));
        let Ok(set) = oracle_text.parse::<rrule::RRuleSet>() else {
            continue;
        };
        let result = set.all(400);
        let mut expected: Vec<DateTime<Utc>> = result
            .dates
            .iter()
            .map(|d| d.with_timezone(&Utc))
            .filter(|d| *d < window_end)
            .collect();
        expected.push(start);
        expected.sort();
        expected.dedup();

        let mut event = Event::new(
            "diff",
            EventTime::Utc { at: start },
            EventTime::Utc { at: start },
        );
        event.rrule = Some(rule.clone());
        let options = ExpandOptions {
            max_occurrences: 400,
            ..ExpandOptions::default()
        };
        let mine = expand(&event, &[], start, window_end, &TzContext::new(), &options);
        let got: Vec<DateTime<Utc>> = mine.occurrences.iter().map(|o| o.start).collect();

        let n = got.len().min(expected.len());
        // When the oracle hit its iteration limit, only the common prefix counts.
        if result.limited || mine.truncated {
            assert_eq!(got[..n], expected[..n], "rule {rule} from {start}");
        } else {
            assert_eq!(got, expected, "rule {rule} from {start}");
        }
        compared += 1;
    }
    assert!(compared > 2_000, "only {compared} rules were comparable");
}

fn by_uid(data: &envelope_ical::CalendarData, uid: &str) -> (Event, Vec<Event>) {
    let master = data
        .events
        .iter()
        .find(|e| e.uid == uid && e.recurrence_id.is_none())
        .cloned()
        .expect("master");
    let overrides = data
        .events
        .iter()
        .filter(|e| e.uid == uid && e.recurrence_id.is_some())
        .cloned()
        .collect();
    (master, overrides)
}

fn starts(expansion: &envelope_ical::Expansion) -> Vec<String> {
    expansion
        .occurrences
        .iter()
        .map(|o| fmt(&o.start))
        .collect()
}

#[test]
fn new_york_series_with_overrides_across_dst() {
    let data =
        parse_calendar(text(include_bytes!("fixtures/recurring_dst_ny.ics"))).expect("calendar");
    let options = ExpandOptions::with_default_tz(Tz::America__New_York);
    // March 2024 in New York.
    let (from, to) = (utc("2024-03-01 05:00"), utc("2024-04-01 04:00"));

    let (master, overrides) = by_uid(&data, "one-on-one@example.com");
    let march = expand(&master, &overrides, from, to, &data.tz, &options);
    assert_eq!(
        starts(&march),
        [
            "2024-03-01 16:00", // Feb 28 moved into the window (Friday 11:00 EST)
            "2024-03-04 14:00",
            "2024-03-06 14:00",
            "2024-03-11 13:00", // after the DST switch: still 09:00 local
            "2024-03-18 13:00",
            "2024-03-25 13:00",
            // Mar 13 cancelled, Mar 20 EXDATE, Mar 27 moved to April,
            // the Mar 21 override is not an instance and is ignored.
        ]
    );
    let first = &march.occurrences[0];
    assert_eq!(
        first.override_index.map(|i| overrides[i].summary.as_str()),
        Some("1:1 with Sam (moved to Friday)")
    );
    assert_eq!(
        first.recurrence_id,
        Some(EventTime::Zoned {
            local: dt("2024-02-28 09:00"),
            tzid: NY.into()
        })
    );
    assert_eq!(
        first.start_time,
        EventTime::Zoned {
            local: dt("2024-03-01 11:00"),
            tzid: NY.into()
        }
    );
    let regular = &march.occurrences[3];
    assert_eq!(regular.override_index, None);
    assert_eq!(
        regular.recurrence_id,
        Some(EventTime::Zoned {
            local: dt("2024-03-11 09:00"),
            tzid: NY.into()
        })
    );
    assert_eq!(
        regular.end_time,
        EventTime::Zoned {
            local: dt("2024-03-11 09:30"),
            tzid: NY.into()
        }
    );
    assert_eq!(fmt(&regular.end), "2024-03-11 13:30");

    let april = expand(
        &master,
        &overrides,
        utc("2024-04-01 04:00"),
        utc("2024-05-01 04:00"),
        &data.tz,
        &options,
    );
    assert_eq!(
        starts(&april),
        [
            "2024-04-01 13:00",
            "2024-04-02 14:00", // the Mar 27 instance moved here
            "2024-04-03 13:00",
            "2024-04-08 13:00",
            "2024-04-10 13:00",
            "2024-04-15 13:00",
            "2024-04-17 13:00", // UNTIL is inclusive
        ]
    );

    let (trash, _) = by_uid(&data, "trash-day@example.com");
    let trash = expand(&trash, &[], from, to, &data.tz, &options);
    assert_eq!(
        starts(&trash),
        [
            "2024-03-07 05:00",
            "2024-03-14 04:00",
            "2024-03-21 04:00",
            "2024-03-28 04:00"
        ]
    );
    assert!(
        trash
            .occurrences
            .iter()
            .all(|o| o.start_time.is_date() && o.end_time.is_date())
    );

    let (standup, _) = by_uid(&data, "standup@example.com");
    let standup = expand(&standup, &[], from, to, &data.tz, &options);
    assert_eq!(
        starts(&standup),
        [
            "2024-03-07 13:30",
            "2024-03-08 13:30",
            "2024-03-09 13:30",
            "2024-03-10 12:30",
            "2024-03-11 12:30",
            "2024-03-12 12:30",
            "2024-03-13 12:30",
            "2024-03-14 12:30",
            "2024-03-15 12:30",
            "2024-03-16 12:30",
        ]
    );
    assert!(
        standup
            .occurrences
            .iter()
            .all(|o| o.end - o.start == Duration::minutes(15))
    );

    let (month_end, _) = by_uid(&data, "month-end@example.com");
    let year = expand(
        &month_end,
        &[],
        utc("2024-01-01 00:00"),
        utc("2025-01-01 00:00"),
        &data.tz,
        &options,
    );
    let local_days: Vec<String> = year
        .occurrences
        .iter()
        .map(|o| wall_clock(&o.start_time).format("%m-%d").to_string())
        .collect();
    assert_eq!(
        local_days,
        [
            "01-31", "02-29", "03-29", "04-30", "05-31", "06-28", "07-31", "08-30", "09-30",
            "10-31", "11-29", "12-31"
        ]
    );

    // expand_events groups by UID and attributes overrides to their event.
    let all = expand_events(&data.events, from, to, &data.tz, &options);
    assert_eq!(all.occurrences.len(), 6 + 4 + 10 + 1);
    let moved = all
        .occurrences
        .iter()
        .find(|o| fmt(&o.occurrence.start) == "2024-03-01 16:00")
        .expect("moved occurrence");
    assert_eq!(
        data.events[moved.event_index].summary,
        "1:1 with Sam (moved to Friday)"
    );
    assert!(
        all.occurrences
            .windows(2)
            .all(|w| w[0].occurrence.start <= w[1].occurrence.start)
    );
}

#[test]
fn berlin_series_with_utc_recurrence_ids_and_floating_times() {
    let data =
        parse_calendar(text(include_bytes!("fixtures/recurring_berlin.ics"))).expect("calendar");
    let options = ExpandOptions::with_default_tz(Tz::Europe__Istanbul);
    let (from, to) = (utc("2024-10-20 00:00"), utc("2024-11-04 00:00"));
    let (daily, overrides) = by_uid(&data, "daily-berlin@example.com");
    let expansion = expand(&daily, &overrides, from, to, &data.tz, &options);
    assert_eq!(
        starts(&expansion),
        [
            "2024-10-21 08:00",
            "2024-10-22 08:00",
            "2024-10-23 08:00",
            "2024-10-24 12:00", // override: 14:00 CEST
            "2024-10-25 08:00",
            "2024-10-26 08:00",
            "2024-10-27 09:00", // CET from here on: still 10:00 local
            "2024-10-28 09:00",
            // Oct 29 cancelled by an override whose RECURRENCE-ID is in UTC.
            "2024-10-30 09:00",
        ]
    );

    let (anchored, _) = by_uid(&data, "utc-weekly@example.com");
    let expansion = expand(&anchored, &[], from, to, &data.tz, &options);
    assert_eq!(
        starts(&expansion),
        ["2024-10-21 09:00", "2024-10-28 09:00"],
        "UTC series ignore DST"
    );

    let (floating, _) = by_uid(&data, "floating@example.com");
    let in_istanbul = expand(&floating, &[], from, to, &data.tz, &options);
    assert_eq!(starts(&in_istanbul), ["2024-10-25 15:00"]);
    let in_new_york = expand(
        &floating,
        &[],
        from,
        to,
        &data.tz,
        &ExpandOptions::with_default_tz(Tz::America__New_York),
    );
    assert_eq!(starts(&in_new_york), ["2024-10-25 22:00"]);
    assert_eq!(
        in_new_york.occurrences[0].recurrence_id, None,
        "single events have no recurrence id"
    );
}

#[test]
fn all_day_series_and_until_mismatches() {
    let data =
        parse_calendar(text(include_bytes!("fixtures/allday_multiday.ics"))).expect("calendar");
    let local_dates = |uid: &str, tz: Tz| -> Vec<u32> {
        let (master, _) = by_uid(&data, uid);
        expand(
            &master,
            &[],
            utc("2024-03-01 00:00"),
            utc("2024-04-01 00:00"),
            &data.tz,
            &ExpandOptions::with_default_tz(tz),
        )
        .occurrences
        .iter()
        .map(|o| wall_clock(&o.start_time).day())
        .collect()
    };
    for tz in [
        Tz::Europe__Berlin,
        Tz::America__New_York,
        Tz::Europe__Istanbul,
    ] {
        assert_eq!(
            local_dates("until-midnight-utc@example.com", tz),
            [11, 12, 13, 14, 15],
            "{tz}"
        );
        assert_eq!(
            local_dates("until-end-of-day@example.com", tz),
            [11, 12, 13, 14, 15],
            "{tz}"
        );
    }
    // UNTIL written as local midnight converted to UTC (UTC+1 producer).
    assert_eq!(
        local_dates("until-local-midnight@example.com", Tz::Europe__Berlin),
        [11, 12, 13, 14, 15]
    );
    assert_eq!(
        local_dates("until-local-midnight@example.com", Tz::America__New_York),
        [11, 12, 13, 14]
    );
    // A DATE UNTIL on a timed series includes the whole day.
    assert_eq!(
        local_dates("timed-date-until@example.com", Tz::UTC),
        [11, 12, 13]
    );

    let (birthday, _) = by_uid(&data, "birthday@example.com");
    let years = expand(
        &birthday,
        &[],
        utc("2023-01-01 00:00"),
        utc("2030-01-01 00:00"),
        &data.tz,
        &ExpandOptions::default(),
    );
    let found: Vec<String> = years
        .occurrences
        .iter()
        .map(|o| wall_clock(&o.start_time).format("%Y-%m-%d").to_string())
        .collect();
    assert_eq!(
        found,
        ["2024-02-29", "2028-02-29"],
        "invalid dates are skipped, not clamped"
    );

    // A multi-day all-day event overlapping the window start is included.
    let (vacation, _) = by_uid(&data, "vacation@example.com");
    let overlap = expand(
        &vacation,
        &[],
        utc("2024-08-01 00:00"),
        utc("2024-08-02 00:00"),
        &data.tz,
        &ExpandOptions::default(),
    );
    assert_eq!(overlap.occurrences.len(), 1);
    assert_eq!(fmt(&overlap.occurrences[0].end), "2024-08-03 00:00");
}

#[test]
fn custom_vtimezone_series_follow_their_definition() {
    let data = parse_calendar(text(include_bytes!("fixtures/custom_tz.ics"))).expect("calendar");
    let (master, _) = by_uid(&data, "mars-weekly@example.com");
    let expansion = expand(
        &master,
        &[],
        utc("2024-04-01 00:00"),
        utc("2024-06-01 00:00"),
        &data.tz,
        &ExpandOptions::default(),
    );
    assert_eq!(
        starts(&expansion),
        [
            "2024-04-22 03:15",
            "2024-04-29 03:15",
            "2024-05-06 02:15",
            "2024-05-13 02:15"
        ]
    );
}

#[test]
fn windows_and_display_name_series() {
    let data = parse_calendar(text(include_bytes!("fixtures/outlook_display_tzid.ics")))
        .expect("calendar");
    let expansion = expand(
        &data.events[0],
        &[],
        utc("2024-01-01 00:00"),
        utc("2024-12-31 00:00"),
        &data.tz,
        &ExpandOptions::default(),
    );
    assert_eq!(
        starts(&expansion),
        [
            "2024-01-08 09:00",
            "2024-01-22 09:00",
            "2024-02-05 09:00",
            "2024-02-19 09:00",
            "2024-03-04 09:00",
            "2024-03-18 09:00",
            "2024-04-01 08:00"
        ]
    );

    let data = parse_calendar(text(include_bytes!("fixtures/istanbul.ics"))).expect("calendar");
    let expansion = expand(
        &data.events[0],
        &[],
        utc("2015-01-01 00:00"),
        utc("2018-01-01 00:00"),
        &data.tz,
        &ExpandOptions::default(),
    );
    assert_eq!(
        starts(&expansion),
        [
            "2015-01-05 08:00", // EET (UTC+2)
            "2015-07-06 07:00", // EEST (UTC+3)
            "2016-01-04 08:00",
            "2016-07-04 07:00",
            "2017-01-02 07:00", // permanent UTC+3 since September 2016
            "2017-07-03 07:00",
        ]
    );
}

#[test]
fn dtstart_not_matching_the_rule_is_still_the_first_instance() {
    // DTSTART is a Wednesday, the rule only produces Mondays; COUNT includes DTSTART.
    let e = ny_series("2024-01-03 09:00", "FREQ=WEEKLY;BYDAY=MO;COUNT=3");
    assert_eq!(
        locals(&e, "2024-01-01 00:00", "2024-03-01 00:00"),
        ["2024-01-03 09:00", "2024-01-08 09:00", "2024-01-15 09:00"]
    );
}

#[test]
fn rdates_exdates_and_periods() {
    let mut e = ny_series("2024-01-01 09:00", "FREQ=WEEKLY;COUNT=3");
    e.rdates.push(EventTime::Zoned {
        local: dt("2024-01-04 15:00"),
        tzid: NY.into(),
    });
    e.rdates.push(EventTime::Utc {
        at: utc("2024-01-05 14:00"),
    });
    e.rdates.push(EventTime::Date {
        date: common::date("2024-01-06"),
    });
    e.exdates.push(EventTime::Utc {
        at: utc("2024-01-08 14:00"),
    });
    assert_eq!(
        locals(&e, "2024-01-01 00:00", "2024-02-01 00:00"),
        [
            "2024-01-01 09:00",
            "2024-01-04 15:00",
            "2024-01-05 09:00",
            "2024-01-06 09:00",
            "2024-01-15 09:00"
        ]
    );

    // An all-day EXDATE on a timed series removes that day's instance.
    let mut e = ny_series("2024-01-01 09:00", "FREQ=DAILY;COUNT=3");
    e.exdates.push(EventTime::Date {
        date: common::date("2024-01-02"),
    });
    assert_eq!(
        locals(&e, "2024-01-01 00:00", "2024-02-01 00:00"),
        ["2024-01-01 09:00", "2024-01-03 09:00"]
    );

    // PERIOD values in RDATE are tolerated (their start is used).
    let text = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:p\r\nDTSTART:20240101T100000Z\r\nDTEND:20240101T110000Z\r\nRDATE;VALUE=PERIOD:20240105T100000Z/20240105T120000Z,20240107T100000Z/PT2H\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let data = parse_calendar(text).expect("calendar");
    let expansion = expand(
        &data.events[0],
        &[],
        utc("2024-01-01 00:00"),
        utc("2024-02-01 00:00"),
        &data.tz,
        &ExpandOptions::default(),
    );
    assert_eq!(
        starts(&expansion),
        ["2024-01-01 10:00", "2024-01-05 10:00", "2024-01-07 10:00"]
    );
}

#[test]
fn overrides_matching_across_value_forms() {
    // All-day series with an override whose RECURRENCE-ID was written as a date-time.
    let text = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20240101\r\nRRULE:FREQ=DAILY;COUNT=3\r\nSUMMARY:Series\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:a\r\nRECURRENCE-ID:20240102T000000Z\r\nDTSTART;VALUE=DATE:20240102\r\nSUMMARY:Changed\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let data = parse_calendar(text).expect("calendar");
    let all = expand_events(
        &data.events,
        utc("2024-01-01 00:00"),
        utc("2024-01-10 00:00"),
        &data.tz,
        &ExpandOptions::default(),
    );
    let summaries: Vec<&str> = all
        .occurrences
        .iter()
        .map(|o| data.events[o.event_index].summary.as_str())
        .collect();
    assert_eq!(summaries, ["Series", "Changed", "Series"]);
}

#[test]
fn masterless_overrides_are_standalone() {
    let text = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:invited-to-one\r\nRECURRENCE-ID;TZID=Europe/Berlin:20240305T100000\r\nDTSTART;TZID=Europe/Berlin:20240305T110000\r\nDTEND;TZID=Europe/Berlin:20240305T120000\r\nSUMMARY:Only this one\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let data = parse_calendar(text).expect("calendar");
    let all = expand_events(
        &data.events,
        utc("2024-03-01 00:00"),
        utc("2024-04-01 00:00"),
        &data.tz,
        &ExpandOptions::default(),
    );
    assert_eq!(all.occurrences.len(), 1);
    assert_eq!(
        fmt(&all.occurrences[0].occurrence.start),
        "2024-03-05 10:00"
    );
    assert!(all.occurrences[0].occurrence.recurrence_id.is_some());
}

#[test]
fn unparseable_rrule_still_shows_overrides() {
    let mut master = ny_series("2024-01-01 09:00", "THIS IS NOT A RULE");
    master.uid = "x".into();
    let mut moved = Event::new(
        "x",
        EventTime::Zoned {
            local: dt("2024-01-09 10:00"),
            tzid: NY.into(),
        },
        EventTime::Zoned {
            local: dt("2024-01-09 11:00"),
            tzid: NY.into(),
        },
    );
    moved.recurrence_id = Some(EventTime::Zoned {
        local: dt("2024-01-08 09:00"),
        tzid: NY.into(),
    });
    let mut cancelled = moved.clone();
    cancelled.recurrence_id = Some(EventTime::Zoned {
        local: dt("2024-01-15 09:00"),
        tzid: NY.into(),
    });
    cancelled.status = EventStatus::Cancelled;
    let expansion = expand(
        &master,
        &[moved, cancelled],
        utc("2024-01-01 00:00"),
        utc("2024-02-01 00:00"),
        &TzContext::new(),
        &ExpandOptions::default(),
    );
    assert_eq!(starts(&expansion), ["2024-01-01 14:00", "2024-01-09 15:00"]);
}

#[test]
fn hard_caps_and_pathological_rules() {
    // Cap on returned occurrences.
    let e = ny_series("2024-01-01 00:00", "FREQ=MINUTELY");
    let options = ExpandOptions {
        max_occurrences: 100,
        ..ExpandOptions::default()
    };
    let capped = expand(
        &e,
        &[],
        utc("2024-01-01 00:00"),
        utc("2025-01-01 00:00"),
        &TzContext::new(),
        &options,
    );
    assert_eq!(capped.occurrences.len(), 100);
    assert!(capped.truncated);

    // Rules that never produce anything terminate.
    for rule in [
        "FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30",
        "FREQ=MONTHLY;BYMONTHDAY=31;BYMONTH=4,6,9,11",
        "FREQ=DAILY;BYMONTH=2;BYMONTHDAY=30",
        "FREQ=WEEKLY;BYMONTH=2;BYMONTHDAY=31",
    ] {
        let e = ny_series("2024-01-01 09:00", rule);
        let expansion = expand(
            &e,
            &[],
            utc("2024-01-02 00:00"),
            utc("2030-01-01 00:00"),
            &TzContext::new(),
            &ExpandOptions::default(),
        );
        assert!(expansion.occurrences.len() <= 1, "{rule}");
    }

    // A dense rule started long before the window finishes quickly.
    let e = ny_series("1990-01-01 00:00", "FREQ=SECONDLY;INTERVAL=7");
    let started = std::time::Instant::now();
    let expansion = expand(
        &e,
        &[],
        utc("2026-01-01 00:00"),
        utc("2026-01-02 00:00"),
        &TzContext::new(),
        &ExpandOptions::default(),
    );
    assert!(expansion.truncated);
    assert!(started.elapsed() < std::time::Duration::from_secs(30));

    // Long daily series skip ahead instead of iterating from 1970.
    let e = ny_series("1970-01-01 09:00", "FREQ=DAILY");
    let expansion = expand(
        &e,
        &[],
        utc("2026-03-01 00:00"),
        utc("2026-03-08 00:00"),
        &TzContext::new(),
        &ExpandOptions::default(),
    );
    assert_eq!(expansion.occurrences.len(), 7);
    assert!(!expansion.truncated);

    // Empty or inverted windows.
    let expansion = expand(
        &e,
        &[],
        utc("2026-03-08 00:00"),
        utc("2026-03-01 00:00"),
        &TzContext::new(),
        &ExpandOptions::default(),
    );
    assert!(expansion.occurrences.is_empty());
}

#[test]
fn zero_length_and_window_edges() {
    let at = utc("2024-05-01 12:00");
    let event = Event::new("z", EventTime::Utc { at }, EventTime::Utc { at });
    let inside = expand(
        &event,
        &[],
        at,
        at + Duration::hours(1),
        &TzContext::new(),
        &ExpandOptions::default(),
    );
    assert_eq!(
        inside.occurrences.len(),
        1,
        "zero-length events at the window start are included"
    );
    let before = expand(
        &event,
        &[],
        at - Duration::hours(1),
        at,
        &TzContext::new(),
        &ExpandOptions::default(),
    );
    assert!(before.occurrences.is_empty(), "the window end is exclusive");

    let start: NaiveDateTime = dt("2024-05-01 10:00");
    let mut event = Event::new(
        "w",
        EventTime::Zoned {
            local: start,
            tzid: "Europe/Istanbul".into(),
        },
        EventTime::Zoned {
            local: start + Duration::hours(2),
            tzid: "Europe/Istanbul".into(),
        },
    );
    event.rrule = Some("FREQ=DAILY;COUNT=2".into());
    // Overlapping the window start by the event's duration.
    let partial = expand(
        &event,
        &[],
        utc("2024-05-01 08:00"),
        utc("2024-05-01 09:00"),
        &TzContext::new(),
        &ExpandOptions::default(),
    );
    assert_eq!(starts(&partial), ["2024-05-01 07:00"]);
}
