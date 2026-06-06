//! Rolling-period summaries (PRD §3.3; ADR-101).
//!
//! Pure: [`RollingPeriod::window`] computes the period window containing an
//! instant (DST-correct, in the schedule's zone), and [`decide_rolling`] is the
//! accumulation state machine — start a new period, accumulate the days since
//! the last run (catching up missed days), or finalize once the period ends.
//! The one-active-summary-per-schedule invariant is honored by feeding in only
//! the active (non-finalized) summary; storage/LLM merging is host-side.

use chrono::{Datelike, Duration, NaiveDate, TimeZone, Weekday};
use chrono_tz::Tz;

/// Rolling cadence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollingPeriod {
    Weekly,
    /// Epoch-aligned 14-day tiles (deterministic; `end_day` doesn't apply).
    Biweekly,
    /// Calendar month.
    Monthly,
}

/// How daily content is merged into the rolling summary (ADR-101). Carried
/// through to the host's accumulation step; pure here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccumulationStrategy {
    /// Each run appends a dated section.
    Append,
    /// Re-summarize all accumulated content each run.
    Resummarize,
    /// Merge highlights + dedup structured data (recommended).
    Hybrid,
}

/// A half-open period window `[start, end)` in UTC seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeriodWindow {
    pub start: i64,
    pub end: i64,
}

/// The active (non-finalized) rolling summary's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollingState {
    pub period_start: i64,
    pub period_end: i64,
    /// Messages up to here have been accumulated.
    pub accumulated_through: i64,
    pub finalized: bool,
    pub accumulation_count: u32,
}

/// What a rolling run should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollingAction {
    /// No active period: open one for `window` and accumulate from its start.
    StartNew { window: PeriodWindow, until: i64 },
    /// Accumulate messages in `(since, until]` into the active summary (catches
    /// up any missed days since the last run).
    Accumulate { since: i64, until: i64 },
    /// The active period has ended — finalize it (idempotent; the next run starts
    /// a fresh period).
    Finalize,
}

impl RollingPeriod {
    /// The period window containing `instant`, resolved in `tz`. `end_day` is the
    /// weekday a weekly period ends on (ignored for biweekly/monthly).
    pub fn window(self, instant: i64, tz: Tz, end_day: Weekday) -> Option<PeriodWindow> {
        let today = tz.timestamp_opt(instant, 0).single()?.date_naive();
        let (start_date, end_excl) = match self {
            RollingPeriod::Weekly => {
                let cur = today.weekday().num_days_from_monday();
                let target = end_day.num_days_from_monday();
                let to_end = (target + 7 - cur) % 7;
                let end_date = today + Duration::days(i64::from(to_end));
                (end_date - Duration::days(6), end_date + Duration::days(1))
            }
            RollingPeriod::Biweekly => {
                // Tile the calendar into 14-day windows from a fixed Monday.
                let epoch = NaiveDate::from_ymd_opt(1970, 1, 5)?; // Monday
                let tile = (today - epoch).num_days().div_euclid(14);
                let start = epoch + Duration::days(tile * 14);
                (start, start + Duration::days(14))
            }
            RollingPeriod::Monthly => {
                let start = NaiveDate::from_ymd_opt(today.year(), today.month(), 1)?;
                let (ny, nm) = if today.month() == 12 {
                    (today.year() + 1, 1)
                } else {
                    (today.year(), today.month() + 1)
                };
                (start, NaiveDate::from_ymd_opt(ny, nm, 1)?)
            }
        };
        Some(PeriodWindow {
            start: start_of_day(start_date, tz)?,
            end: start_of_day(end_excl, tz)?,
        })
    }
}

/// Midnight (00:00) of `date` in `tz`, as a UTC timestamp.
fn start_of_day(date: NaiveDate, tz: Tz) -> Option<i64> {
    let naive = date.and_hms_opt(0, 0, 0)?;
    tz.from_local_datetime(&naive)
        .earliest()
        .map(|dt| dt.timestamp())
}

/// Decide the action for a rolling run at `now`, given the active rolling summary
/// (if any) and the `window` containing `now`.
pub fn decide_rolling(
    active: Option<&RollingState>,
    window: PeriodWindow,
    now: i64,
) -> RollingAction {
    match active {
        // No active period → open one and accumulate from its start to now.
        None => RollingAction::StartNew { window, until: now },
        Some(state) => {
            if now >= state.period_end {
                // The active period is over → finalize (next run starts fresh).
                RollingAction::Finalize
            } else {
                // Still in-period → accumulate since the last run (catch-up).
                RollingAction::Accumulate {
                    since: state.accumulated_through,
                    until: now,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    const LONDON: Tz = chrono_tz::Europe::London;

    fn utc(y: i32, mo: u32, d: u32, h: u32) -> i64 {
        Utc.with_ymd_and_hms(y, mo, d, h, 0, 0).unwrap().timestamp()
    }

    #[test]
    fn weekly_window_ends_on_end_day() {
        // 2026-01-01 is a Thursday; week ending Sunday → Mon 2025-12-29 .. Mon 2026-01-05.
        let w = RollingPeriod::Weekly
            .window(utc(2026, 1, 1, 12), LONDON, Weekday::Sun)
            .unwrap();
        assert_eq!(w.start, utc(2025, 12, 29, 0));
        assert_eq!(w.end, utc(2026, 1, 5, 0));
        // 7-day span.
        assert_eq!(w.end - w.start, 7 * 86_400);
    }

    #[test]
    fn monthly_window_is_calendar_month() {
        let w = RollingPeriod::Monthly
            .window(utc(2026, 2, 15, 9), LONDON, Weekday::Sun)
            .unwrap();
        assert_eq!(w.start, utc(2026, 2, 1, 0));
        assert_eq!(w.end, utc(2026, 3, 1, 0));
    }

    #[test]
    fn biweekly_tiles_are_14_days_and_stable() {
        let t = utc(2026, 1, 1, 0);
        let w = RollingPeriod::Biweekly
            .window(t, LONDON, Weekday::Sun)
            .unwrap();
        assert_eq!(w.end - w.start, 14 * 86_400); // no DST in January
        assert!(w.start <= t && t < w.end); // window contains the instant
                                            // Any instant inside the tile resolves to the same window (boundary-safe).
        assert_eq!(
            RollingPeriod::Biweekly.window(w.start, LONDON, Weekday::Sun),
            Some(w)
        );
        assert_eq!(
            RollingPeriod::Biweekly.window(w.end - 1, LONDON, Weekday::Sun),
            Some(w)
        );
    }

    fn window() -> PeriodWindow {
        PeriodWindow {
            start: 1_000,
            end: 8_000,
        }
    }

    #[test]
    fn no_active_summary_starts_a_new_period() {
        assert_eq!(
            decide_rolling(None, window(), 2_000),
            RollingAction::StartNew {
                window: window(),
                until: 2_000
            }
        );
    }

    #[test]
    fn in_period_run_accumulates_since_last_run() {
        let state = RollingState {
            period_start: 1_000,
            period_end: 8_000,
            accumulated_through: 3_000,
            finalized: false,
            accumulation_count: 2,
        };
        // Catches up everything since the last accumulation (missed days).
        assert_eq!(
            decide_rolling(Some(&state), window(), 5_000),
            RollingAction::Accumulate {
                since: 3_000,
                until: 5_000
            }
        );
    }

    #[test]
    fn run_after_period_end_finalizes() {
        let state = RollingState {
            period_start: 1_000,
            period_end: 8_000,
            accumulated_through: 7_000,
            finalized: false,
            accumulation_count: 6,
        };
        assert_eq!(
            decide_rolling(Some(&state), window(), 8_000),
            RollingAction::Finalize
        );
        assert_eq!(
            decide_rolling(Some(&state), window(), 9_999),
            RollingAction::Finalize
        );
    }
}
