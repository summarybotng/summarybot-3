//! WhatsApp timestamp parsing + DST-correct UTC normalization (WHA-020).
//!
//! Export timestamps are local wall-clock with no offset in the file, and the
//! date-component order is locale-dependent. We parse the numeric components
//! against a declared [`DateOrder`], then resolve the local time in a declared
//! IANA zone to a UTC instant — using the **historical** offset for that date,
//! so summer/winter (DST) conversions are correct.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use chrono_tz::Tz;

/// Which order the date components appear in (the export doesn't say, so the
/// uploader/host declares it, like the timezone).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateOrder {
    /// `DD/MM/YY` — most of the world.
    DayMonthYear,
    /// `MM/DD/YY` — US locale.
    MonthDayYear,
}

/// Parse a `"<date>, <time>"` stamp into a naive (zone-less) datetime.
pub(super) fn parse_stamp(stamp: &str, order: DateOrder) -> Option<NaiveDateTime> {
    let (date_part, time_part) = stamp.split_once(',')?;
    let date = parse_date(date_part.trim(), order)?;
    let time = parse_time(time_part.trim())?;
    Some(NaiveDateTime::new(date, time))
}

/// Resolve a naive local datetime to a UTC unix timestamp in `zone`, using that
/// date's historical offset. Ambiguous times (DST fall-back) take the earlier
/// instant; nonexistent times (spring-forward gap) yield `None`.
pub(super) fn to_utc(naive: NaiveDateTime, zone: Tz) -> Option<i64> {
    zone.from_local_datetime(&naive)
        .earliest()
        .map(|dt| dt.timestamp())
}

fn parse_date(s: &str, order: DateOrder) -> Option<NaiveDate> {
    let parts: Vec<i32> = s
        .split(['/', '.', '-'])
        .map(|p| p.trim().parse::<i32>())
        .collect::<Result<_, _>>()
        .ok()?;
    if parts.len() != 3 {
        return None;
    }
    let (day, month, year) = match order {
        DateOrder::DayMonthYear => (parts[0], parts[1], parts[2]),
        DateOrder::MonthDayYear => (parts[1], parts[0], parts[2]),
    };
    let year = if (0..100).contains(&year) {
        year + 2000
    } else {
        year
    };
    NaiveDate::from_ymd_opt(year, u32::try_from(month).ok()?, u32::try_from(day).ok()?)
}

fn parse_time(s: &str) -> Option<NaiveTime> {
    let up = s.to_uppercase();
    let (digits, pm) = if let Some(p) = up.strip_suffix("AM") {
        (p.trim(), Some(false))
    } else if let Some(p) = up.strip_suffix("PM") {
        (p.trim(), Some(true))
    } else {
        (up.as_str(), None)
    };
    let mut it = digits.split(':');
    let mut hour: u32 = it.next()?.trim().parse().ok()?;
    let minute: u32 = it.next()?.trim().parse().ok()?;
    let second: u32 = match it.next() {
        Some(s) => s.trim().parse().ok()?,
        None => 0,
    };
    hour = match pm {
        Some(true) if hour < 12 => hour + 12, // 1–11 PM
        Some(false) if hour == 12 => 0,       // 12 AM = midnight
        _ => hour,
    };
    NaiveTime::from_hms_opt(hour, minute, second)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn expect_utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s)
            .unwrap()
            .timestamp()
    }

    #[test]
    fn parses_dmy_24h_with_seconds() {
        let dt = parse_stamp("01/02/2026, 09:15:32", DateOrder::DayMonthYear).unwrap();
        assert_eq!(dt.to_string(), "2026-02-01 09:15:32"); // 1 Feb, not 2 Jan
    }

    #[test]
    fn parses_mdy_and_two_digit_year() {
        let dt = parse_stamp("02/01/26, 09:15", DateOrder::MonthDayYear).unwrap();
        assert_eq!(dt.to_string(), "2026-02-01 09:15:00");
    }

    #[test]
    fn parses_12h_ampm() {
        let pm = parse_stamp("01/01/2026, 1:05:00 PM", DateOrder::DayMonthYear).unwrap();
        assert_eq!(pm.time(), NaiveTime::from_hms_opt(13, 5, 0).unwrap());
        let midnight = parse_stamp("01/01/2026, 12:00 AM", DateOrder::DayMonthYear).unwrap();
        assert_eq!(midnight.time(), NaiveTime::from_hms_opt(0, 0, 0).unwrap());
        let noon = parse_stamp("01/01/2026, 12:00 PM", DateOrder::DayMonthYear).unwrap();
        assert_eq!(noon.time(), NaiveTime::from_hms_opt(12, 0, 0).unwrap());
    }

    #[test]
    fn utc_conversion_is_dst_correct() {
        let london = chrono_tz::Europe::London;
        // Winter (GMT, +0): noon local == noon UTC.
        let winter = parse_stamp("01/01/2026, 12:00:00", DateOrder::DayMonthYear).unwrap();
        assert_eq!(
            to_utc(winter, london),
            Some(expect_utc(2026, 1, 1, 12, 0, 0))
        );
        // Summer (BST, +1): noon local == 11:00 UTC.
        let summer = parse_stamp("01/07/2026, 12:00:00", DateOrder::DayMonthYear).unwrap();
        assert_eq!(
            to_utc(summer, london),
            Some(expect_utc(2026, 7, 1, 11, 0, 0))
        );
    }

    #[test]
    fn same_instant_in_two_zones_normalizes_equal() {
        // The cross-uploader dedup premise (WHA-012): London 12:00 BST and
        // New York 07:00 EDT are the same instant → same UTC timestamp.
        let london = parse_stamp("01/07/2026, 12:00:00", DateOrder::DayMonthYear).unwrap();
        let ny = parse_stamp("01/07/2026, 07:00:00", DateOrder::DayMonthYear).unwrap();
        assert_eq!(
            to_utc(london, chrono_tz::Europe::London),
            to_utc(ny, chrono_tz::America::New_York)
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_stamp("not a stamp", DateOrder::DayMonthYear).is_none());
        assert!(parse_stamp("99/99/2026, 12:00", DateOrder::DayMonthYear).is_none());
    }
}
