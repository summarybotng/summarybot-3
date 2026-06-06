//! Schedule recurrence + tick policy (PRD §3.1 SCH-*; ADR-011 scope).
//!
//! Pure: given a [`Schedule`] and an instant, [`Schedule::next_run`] computes the
//! next fire time (DST-correct, in the schedule's IANA zone), and
//! [`evaluate_tick`] decides — without I/O — whether a due schedule should fire,
//! wait, skip a stale missed run, or auto-disable after repeated failures. The
//! executor loop (clock, running summaries, persistence) is host-side.

use crate::WorkspaceId;
use chrono::{Datelike, Duration, TimeZone, Weekday};
use chrono_tz::Tz;

/// Recurrence kinds (SCH-001).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleType {
    Once,
    FifteenMinutes,
    Hourly,
    EveryFourHours,
    Daily,
    Weekly,
    /// Twice-weekly cadence (~3.5 day interval).
    HalfWeekly,
    Monthly,
    /// Arbitrary fixed interval (`custom_interval_secs`).
    Custom,
}

/// Wall-clock time of day in the schedule's zone (SCH-002).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeOfDay {
    pub hour: u32,
    pub minute: u32,
}

/// A recurrence definition. Time-of-day/days/day-of-month apply to the
/// wall-clock kinds (Daily/Weekly/Monthly); interval kinds align to epoch
/// multiples; `Once` fires a single `once_at`.
#[derive(Debug, Clone)]
pub struct Schedule {
    pub workspace_id: WorkspaceId,
    pub schedule_type: ScheduleType,
    pub at: TimeOfDay,
    /// Days for `Weekly` (SCH-003).
    pub days: Vec<Weekday>,
    /// Day-of-month (1–28 recommended) for `Monthly`.
    pub day_of_month: u32,
    pub timezone: Tz,
    /// Target instant for `Once`.
    pub once_at: Option<i64>,
    /// Interval (seconds) for `Custom`.
    pub custom_interval_secs: i64,
    pub enabled: bool,
}

impl Schedule {
    /// The next fire time strictly after `after` (unix seconds), or `None` if the
    /// schedule has no future run (a past `Once`, or a misconfiguration).
    pub fn next_run(&self, after: i64) -> Option<i64> {
        match self.schedule_type {
            ScheduleType::Once => self.once_at.filter(|t| *t > after),
            ScheduleType::FifteenMinutes => Some(next_interval(after, 15 * 60)),
            ScheduleType::Hourly => Some(next_interval(after, 60 * 60)),
            ScheduleType::EveryFourHours => Some(next_interval(after, 4 * 60 * 60)),
            ScheduleType::HalfWeekly => Some(next_interval(after, 7 * 24 * 60 * 60 / 2)),
            ScheduleType::Custom => (self.custom_interval_secs > 0)
                .then(|| next_interval(after, self.custom_interval_secs)),
            ScheduleType::Daily | ScheduleType::Weekly | ScheduleType::Monthly => {
                self.next_wallclock(after)
            }
        }
    }

    /// Next wall-clock occurrence for Daily/Weekly/Monthly, scanning forward day
    /// by day (cheap; ≤ ~2 months covers monthly) and converting each candidate
    /// in the schedule's zone (DST-correct).
    fn next_wallclock(&self, after: i64) -> Option<i64> {
        let after_dt = self.timezone.timestamp_opt(after, 0).single()?;
        let start = after_dt.date_naive();
        for offset in 0..=62 {
            let date = start + Duration::days(offset);
            let matches = match self.schedule_type {
                ScheduleType::Daily => true,
                ScheduleType::Weekly => self.days.contains(&date.weekday()),
                ScheduleType::Monthly => date.day() == self.day_of_month,
                _ => false,
            };
            if !matches {
                continue;
            }
            let naive = date.and_hms_opt(self.at.hour, self.at.minute, 0)?;
            if let Some(dt) = self.timezone.from_local_datetime(&naive).earliest() {
                let ts = dt.timestamp();
                if ts > after {
                    return Some(ts);
                }
            }
        }
        None
    }
}

/// Next epoch-aligned multiple of `interval` strictly after `after`.
fn next_interval(after: i64, interval: i64) -> i64 {
    (after / interval + 1) * interval
}

/// What the executor should do with a schedule this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickAction {
    /// Run it now.
    Fire,
    /// Not due yet.
    NotYet,
    /// Due time passed beyond the grace window — skip this run (don't fire a
    /// stale summary), just reschedule. Covers downtime/restart catch-up.
    Skip,
    /// Too many consecutive failures — auto-disable (SCH).
    Disable,
}

/// Decide a schedule's fate at `now`. `grace_secs` is how late a run may fire
/// before it's considered missed; `max_failures` triggers auto-disable.
pub fn evaluate_tick(
    next_run: i64,
    now: i64,
    grace_secs: i64,
    consecutive_failures: u32,
    max_failures: u32,
) -> TickAction {
    if consecutive_failures >= max_failures {
        return TickAction::Disable;
    }
    if now < next_run {
        TickAction::NotYet
    } else if now <= next_run + grace_secs {
        TickAction::Fire
    } else {
        TickAction::Skip
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    const LONDON: Tz = chrono_tz::Europe::London;

    fn schedule(ty: ScheduleType) -> Schedule {
        Schedule {
            workspace_id: WorkspaceId::parse("ws-1").unwrap(),
            schedule_type: ty,
            at: TimeOfDay { hour: 9, minute: 0 },
            days: vec![Weekday::Mon, Weekday::Thu],
            day_of_month: 1,
            timezone: LONDON,
            once_at: None,
            custom_interval_secs: 0,
            enabled: true,
        }
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
            .timestamp()
    }

    #[test]
    fn interval_types_align_to_epoch_multiples() {
        // 15-minute: next multiple of 900s after a non-boundary instant.
        let s = schedule(ScheduleType::FifteenMinutes);
        let after = 1_000; // 900*1 = 900 < 1000 < 1800
        assert_eq!(s.next_run(after), Some(1_800));
        // Exactly on a boundary → the next one (strictly after).
        assert_eq!(s.next_run(1_800), Some(2_700));
    }

    #[test]
    fn hourly_is_top_of_hour() {
        let s = schedule(ScheduleType::Hourly);
        // 3600-aligned.
        assert_eq!(s.next_run(3_700), Some(7_200));
    }

    #[test]
    fn once_fires_once_then_never() {
        let mut s = schedule(ScheduleType::Once);
        s.once_at = Some(5_000);
        assert_eq!(s.next_run(4_000), Some(5_000));
        assert_eq!(s.next_run(5_000), None); // not strictly after
        assert_eq!(s.next_run(6_000), None);
    }

    #[test]
    fn daily_fires_at_local_nine_am_dst_correct() {
        let s = schedule(ScheduleType::Daily);
        // Winter (GMT): 1 Jan 2026, just after midnight UTC → 09:00 local = 09:00 UTC.
        let after = utc(2026, 1, 1, 0, 30);
        assert_eq!(s.next_run(after), Some(utc(2026, 1, 1, 9, 0)));
        // Summer (BST, +1): 1 Jul → 09:00 local = 08:00 UTC.
        let after = utc(2026, 7, 1, 0, 30);
        assert_eq!(s.next_run(after), Some(utc(2026, 7, 1, 8, 0)));
    }

    #[test]
    fn daily_rolls_to_tomorrow_when_past_today() {
        let s = schedule(ScheduleType::Daily);
        // After 09:00 today → fires 09:00 next day.
        let after = utc(2026, 1, 1, 10, 0);
        assert_eq!(s.next_run(after), Some(utc(2026, 1, 2, 9, 0)));
    }

    #[test]
    fn weekly_picks_next_selected_day() {
        let s = schedule(ScheduleType::Weekly); // Mon + Thu, 09:00
                                                // 2026-01-01 is a Thursday. Just after midnight → fires Thu 09:00.
        let thu = utc(2026, 1, 1, 0, 30);
        assert_eq!(s.next_run(thu), Some(utc(2026, 1, 1, 9, 0)));
        // After Thu 09:00 → next is Mon 2026-01-05 09:00.
        let after_thu = utc(2026, 1, 1, 10, 0);
        assert_eq!(s.next_run(after_thu), Some(utc(2026, 1, 5, 9, 0)));
    }

    #[test]
    fn monthly_fires_on_day_of_month() {
        let mut s = schedule(ScheduleType::Monthly);
        s.day_of_month = 15;
        let after = utc(2026, 1, 20, 0, 0); // past the 15th
        assert_eq!(s.next_run(after), Some(utc(2026, 2, 15, 9, 0)));
    }

    #[test]
    fn custom_requires_positive_interval() {
        let mut s = schedule(ScheduleType::Custom);
        assert_eq!(s.next_run(1_000), None);
        s.custom_interval_secs = 600;
        assert_eq!(s.next_run(1_000), Some(1_200));
    }

    #[test]
    fn tick_fires_within_grace_skips_when_stale() {
        // Due at 1000, grace 300.
        assert_eq!(evaluate_tick(1_000, 900, 300, 0, 3), TickAction::NotYet);
        assert_eq!(evaluate_tick(1_000, 1_000, 300, 0, 3), TickAction::Fire);
        assert_eq!(evaluate_tick(1_000, 1_300, 300, 0, 3), TickAction::Fire);
        assert_eq!(evaluate_tick(1_000, 1_301, 300, 0, 3), TickAction::Skip);
    }

    #[test]
    fn tick_auto_disables_after_max_failures() {
        assert_eq!(evaluate_tick(1_000, 1_000, 300, 3, 3), TickAction::Disable);
        assert_eq!(evaluate_tick(1_000, 1_000, 300, 2, 3), TickAction::Fire);
    }
}
