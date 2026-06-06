//! Summarization decision logic (PRD §12.4, Phase 3 — pure core).
//!
//! Everything here is pure policy/validation the summarization *service* (host)
//! orchestrates around the resilient LLM client:
//!   * [`cost`] — fixed-point cost + hard cap (ADR-095, Q#5);
//!   * [`model`] — start-model + failure-driven fallback (ADR-024);
//!   * [`allocate`] — adaptive token allocation / chunking (ADR-095);
//!   * [`extract`] — structured extraction, grounded-citation resolution, and
//!     structural quality validation (ADR-004, Q#6).
//!
//! The LLM calls, tokenization and JSON parsing are I/O and live host-side; this
//! is the part that must be exhaustively testable without a network.

pub mod allocate;
pub mod cost;
pub mod extract;
pub mod model;

pub use allocate::{allocate, Allocation};
pub use cost::{CostGuard, ModelPrice, SpendDecision};
pub use extract::{
    finalize, ActionItem, ExtractedSummary, FinishReason, QualityError, RawCitation, RawExtraction,
    ResolvedCitation,
};
pub use model::{Model, ModelLadder, NextModel, SummaryLength};
