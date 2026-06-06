//! Model selection + failure-driven fallback (PRD §12.4; ADR-024).
//!
//! Pure policy: pick a *start* model from the requested summary length (cheap
//! for brief, strong for comprehensive), then walk a fallback ladder by failure
//! type — retry transient failures on the same model, escalate on a context/
//! capability problem, give up on a permanent error. The actual call + retry
//! timing is the resilient engine's job; this only decides *which* model next.

use super::cost::ModelPrice;
use crate::failure::FailureClass;

/// How much summary the caller asked for — drives the starting model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryLength {
    Brief,
    Detailed,
    Comprehensive,
}

/// A model the ladder can use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    pub name: String,
    pub price: ModelPrice,
    /// Max context window in tokens (drives allocation + escalation).
    pub context_tokens: i64,
}

/// An ordered, ascending-capability list of models. Index 0 is the cheapest/
/// smallest; the last is the strongest/largest.
#[derive(Debug, Clone)]
pub struct ModelLadder {
    models: Vec<Model>,
}

/// What to do after a failed attempt on `current` (an index into the ladder).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextModel {
    /// Transient failure — retry the same model (the engine handles backoff).
    Retry(usize),
    /// Escalate to a more capable model (e.g. larger context).
    Escalate(usize),
    /// No viable model remains — give up.
    GiveUp,
}

impl ModelLadder {
    /// Build a ladder; must be non-empty (panics otherwise — a config error).
    pub fn new(models: Vec<Model>) -> Self {
        assert!(!models.is_empty(), "model ladder must not be empty");
        Self { models }
    }

    pub fn get(&self, index: usize) -> Option<&Model> {
        self.models.get(index)
    }

    pub fn len(&self) -> usize {
        self.models.len()
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// Index of the model to start with for `length`: brief starts cheapest,
    /// comprehensive starts strongest, detailed splits the difference.
    pub fn start_index(&self, length: SummaryLength) -> usize {
        let last = self.models.len() - 1;
        match length {
            SummaryLength::Brief => 0,
            SummaryLength::Detailed => last / 2,
            SummaryLength::Comprehensive => last,
        }
    }

    /// Decide the next model after a failure of `class` on `current`.
    ///
    /// - permanent (`InvalidRequest`/`QuotaExceeded`/`Unknown`) → give up;
    /// - `ServiceUnavailable` → escalate if a stronger model exists, else retry
    ///   the same (the provider may just be flaky);
    /// - `RateLimited` → retry the same model (the engine/limiter paces it;
    ///   moving models won't help a quota wall).
    pub fn next_after_failure(&self, current: usize, class: FailureClass) -> NextModel {
        let last = self.models.len() - 1;
        match class {
            FailureClass::InvalidRequest | FailureClass::QuotaExceeded | FailureClass::Unknown => {
                NextModel::GiveUp
            }
            FailureClass::RateLimited => NextModel::Retry(current),
            FailureClass::ServiceUnavailable => {
                if current < last {
                    NextModel::Escalate(current + 1)
                } else {
                    NextModel::Retry(current)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn price(i: i64, o: i64) -> ModelPrice {
        ModelPrice {
            input_micros_per_ktoken: i,
            output_micros_per_ktoken: o,
        }
    }

    fn ladder() -> ModelLadder {
        ModelLadder::new(vec![
            Model {
                name: "haiku".into(),
                price: price(800, 4_000),
                context_tokens: 200_000,
            },
            Model {
                name: "sonnet".into(),
                price: price(3_000, 15_000),
                context_tokens: 200_000,
            },
            Model {
                name: "opus".into(),
                price: price(15_000, 75_000),
                context_tokens: 200_000,
            },
        ])
    }

    #[test]
    fn start_model_scales_with_length() {
        let l = ladder();
        assert_eq!(l.start_index(SummaryLength::Brief), 0); // haiku
        assert_eq!(l.start_index(SummaryLength::Detailed), 1); // sonnet
        assert_eq!(l.start_index(SummaryLength::Comprehensive), 2); // opus
    }

    #[test]
    fn permanent_failures_give_up() {
        let l = ladder();
        for c in [
            FailureClass::InvalidRequest,
            FailureClass::QuotaExceeded,
            FailureClass::Unknown,
        ] {
            assert_eq!(l.next_after_failure(0, c), NextModel::GiveUp);
        }
    }

    #[test]
    fn rate_limit_retries_same_model() {
        let l = ladder();
        assert_eq!(
            l.next_after_failure(1, FailureClass::RateLimited),
            NextModel::Retry(1)
        );
    }

    #[test]
    fn service_unavailable_escalates_then_retries_at_top() {
        let l = ladder();
        assert_eq!(
            l.next_after_failure(0, FailureClass::ServiceUnavailable),
            NextModel::Escalate(1)
        );
        assert_eq!(
            l.next_after_failure(2, FailureClass::ServiceUnavailable),
            NextModel::Retry(2)
        );
    }
}
