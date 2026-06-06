//! Fixed-point LLM cost accounting + hard cost cap (PRD §12.4; ADR-095, SUM-016).
//!
//! Money is integer **micro-dollars** (1 USD = 1_000_000 µ$), never floats — so
//! repeated accumulation can't drift. Prices are quoted per 1000 tokens. The
//! [`CostGuard`] enforces a per-request cap with the open-question-#5 policy:
//! when the next call would exceed the cap, stop and let the caller return a
//! **best-effort partial, flagged degraded** (it never silently overspends).

/// Per-model price in micro-dollars per 1000 tokens (input and output).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelPrice {
    pub input_micros_per_ktoken: i64,
    pub output_micros_per_ktoken: i64,
}

impl ModelPrice {
    /// Cost in micro-dollars for a call with the given token counts. Integer
    /// math throughout (per-1000 division last, with the usual truncation).
    pub fn cost_micros(&self, input_tokens: i64, output_tokens: i64) -> i64 {
        let input = input_tokens
            .max(0)
            .saturating_mul(self.input_micros_per_ktoken)
            / 1000;
        let output = output_tokens
            .max(0)
            .saturating_mul(self.output_micros_per_ktoken)
            / 1000;
        input.saturating_add(output)
    }
}

/// Whether the next call fits under the cap (open-question-#5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpendDecision {
    /// The estimated cost fits; proceed.
    Proceed,
    /// The estimate would exceed the cap; stop and degrade gracefully.
    CapReached,
}

/// Tracks spend against a hard cap across a multi-call request (the resilient
/// retry/fallback chain). Check *before* each call; record the *actual* after.
#[derive(Debug, Clone)]
pub struct CostGuard {
    cap_micros: i64,
    spent_micros: i64,
}

impl CostGuard {
    /// A guard with a hard cap (micro-dollars).
    pub fn new(cap_micros: i64) -> Self {
        Self {
            cap_micros: cap_micros.max(0),
            spent_micros: 0,
        }
    }

    /// A guard with no cap (cap = i64::MAX) — checks always proceed.
    pub fn unlimited() -> Self {
        Self {
            cap_micros: i64::MAX,
            spent_micros: 0,
        }
    }

    /// Would spending `estimate_micros` more stay within the cap?
    pub fn check(&self, estimate_micros: i64) -> SpendDecision {
        if self.spent_micros.saturating_add(estimate_micros.max(0)) <= self.cap_micros {
            SpendDecision::Proceed
        } else {
            SpendDecision::CapReached
        }
    }

    /// Record the actual spend of a completed call.
    pub fn record(&mut self, actual_micros: i64) {
        self.spent_micros = self.spent_micros.saturating_add(actual_micros.max(0));
    }

    pub fn spent_micros(&self) -> i64 {
        self.spent_micros
    }

    pub fn cap_micros(&self) -> i64 {
        self.cap_micros
    }

    pub fn remaining_micros(&self) -> i64 {
        (self.cap_micros - self.spent_micros).max(0)
    }

    /// Whether spend has reached or passed the cap.
    pub fn is_exhausted(&self) -> bool {
        self.spent_micros >= self.cap_micros
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRICE: ModelPrice = ModelPrice {
        input_micros_per_ktoken: 3_000,   // $3 / Mtoken in
        output_micros_per_ktoken: 15_000, // $15 / Mtoken out
    };

    #[test]
    fn cost_is_fixed_point_and_per_ktoken() {
        // 2000 in @ $3/Mtok = 6000µ$; 500 out @ $15/Mtok = 7500µ$.
        assert_eq!(PRICE.cost_micros(2_000, 500), 6_000 + 7_500);
    }

    #[test]
    fn cost_truncates_sub_ktoken_fractions() {
        // 1 input token: 1*3000/1000 = 3µ$ (integer division).
        assert_eq!(PRICE.cost_micros(1, 0), 3);
    }

    #[test]
    fn guard_proceeds_until_cap_then_reports_reached() {
        let mut g = CostGuard::new(10_000);
        assert_eq!(g.check(6_000), SpendDecision::Proceed);
        g.record(6_000);
        assert_eq!(g.remaining_micros(), 4_000);
        // Next call estimated at 5_000 would exceed → CapReached (Q#5).
        assert_eq!(g.check(5_000), SpendDecision::CapReached);
        // A smaller call still fits.
        assert_eq!(g.check(4_000), SpendDecision::Proceed);
    }

    #[test]
    fn unlimited_guard_always_proceeds() {
        let g = CostGuard::unlimited();
        assert_eq!(g.check(i64::MAX / 2), SpendDecision::Proceed);
    }

    #[test]
    fn exhaustion_is_reported() {
        let mut g = CostGuard::new(1_000);
        g.record(1_000);
        assert!(g.is_exhausted());
        assert_eq!(g.remaining_micros(), 0);
    }
}
