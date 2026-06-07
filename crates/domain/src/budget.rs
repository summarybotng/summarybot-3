//! Per-tenant LLM budget policy (ADR-125 Phase 3), pure.
//!
//! When the operator lends a tenant the platform key, usage is capped by a
//! budget: a `limit_micros` ceiling over a rolling `period_secs` window. Spend
//! accrues from the `cost_micros` the summarization pipeline already records.
//! This module is the pure policy — rolling the window over, deciding whether a
//! charge is permitted, and computing remaining headroom. Persistence and the
//! actual charge live host/repo-side.
//!
//! Semantics: a call is allowed while accrued spend is *below* the limit; a
//! single call may overshoot (its exact cost isn't known until after), after
//! which further calls are denied until the window rolls.

/// A budget grant: ceiling + window length (seconds). `period_secs == 0` means
/// a non-rolling window (never auto-resets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub limit_micros: i64,
    pub period_secs: i64,
}

/// Persisted spend state for the current window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spend {
    pub period_start: i64,
    pub spent_micros: i64,
}

/// The effective spend window at `now`, after rolling over if the period
/// elapsed. The returned `period_start`/`spent_micros` are what should be
/// persisted on the next charge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub period_start: i64,
    pub spent_micros: i64,
}

/// Roll `spend` forward to the window containing `now`.
pub fn current_window(spend: Spend, budget: Budget, now: i64) -> Window {
    if budget.period_secs > 0 && now >= spend.period_start.saturating_add(budget.period_secs) {
        Window {
            period_start: now,
            spent_micros: 0,
        }
    } else {
        Window {
            period_start: spend.period_start,
            spent_micros: spend.spent_micros,
        }
    }
}

/// Whether a charge is permitted in `window` (spend strictly below the limit).
pub fn within_budget(window: Window, budget: Budget) -> bool {
    window.spent_micros < budget.limit_micros
}

/// Remaining headroom (never negative).
pub fn remaining_micros(window: Window, budget: Budget) -> i64 {
    (budget.limit_micros - window.spent_micros).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: Budget = Budget {
        limit_micros: 1_000,
        period_secs: 100,
    };

    #[test]
    fn within_until_limit_reached() {
        let w = current_window(
            Spend {
                period_start: 0,
                spent_micros: 999,
            },
            B,
            50,
        );
        assert!(within_budget(w, B));
        assert_eq!(remaining_micros(w, B), 1);

        let w = current_window(
            Spend {
                period_start: 0,
                spent_micros: 1_000,
            },
            B,
            50,
        );
        assert!(!within_budget(w, B)); // at the ceiling → denied
        assert_eq!(remaining_micros(w, B), 0);
    }

    #[test]
    fn window_rolls_over_after_period() {
        // Spent out, but the period has elapsed → fresh window, allowed again.
        let spend = Spend {
            period_start: 0,
            spent_micros: 5_000,
        };
        let w = current_window(spend, B, 100); // now == start + period
        assert_eq!(w.period_start, 100);
        assert_eq!(w.spent_micros, 0);
        assert!(within_budget(w, B));
    }

    #[test]
    fn does_not_roll_within_period() {
        let spend = Spend {
            period_start: 10,
            spent_micros: 400,
        };
        let w = current_window(spend, B, 50); // still inside [10, 110)
        assert_eq!(
            w,
            Window {
                period_start: 10,
                spent_micros: 400
            }
        );
    }

    #[test]
    fn zero_period_never_rolls() {
        let nonrolling = Budget {
            limit_micros: 100,
            period_secs: 0,
        };
        let spend = Spend {
            period_start: 0,
            spent_micros: 100,
        };
        let w = current_window(spend, nonrolling, 10_000_000);
        assert_eq!(w.spent_micros, 100); // never reset
        assert!(!within_budget(w, nonrolling));
    }
}
