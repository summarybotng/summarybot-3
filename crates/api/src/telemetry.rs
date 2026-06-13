//! Process-wide request telemetry (PRD §12; ops hardening).
//!
//! Lightweight, dependency-free counters updated by the correlation-id
//! middleware and rendered into the `/metrics` endpoint alongside the DB gauges.
//! Path-agnostic (no per-route labels) so cardinality stays bounded; the
//! per-request *log line* carries the method/path/correlation-id instead.

use std::sync::atomic::{AtomicU64, Ordering};

static REQUESTS: AtomicU64 = AtomicU64::new(0);
static RESP_2XX: AtomicU64 = AtomicU64::new(0);
static RESP_4XX: AtomicU64 = AtomicU64::new(0);
static RESP_5XX: AtomicU64 = AtomicU64::new(0);
static DURATION_MS_SUM: AtomicU64 = AtomicU64::new(0);

/// Record one handled request: its response status and wall-clock duration.
/// 1xx/3xx count toward the total + duration but not a status-class bucket.
pub(crate) fn record(status: u16, duration_ms: u64) {
    REQUESTS.fetch_add(1, Ordering::Relaxed);
    DURATION_MS_SUM.fetch_add(duration_ms, Ordering::Relaxed);
    match status {
        200..=299 => RESP_2XX.fetch_add(1, Ordering::Relaxed),
        400..=499 => RESP_4XX.fetch_add(1, Ordering::Relaxed),
        500..=599 => RESP_5XX.fetch_add(1, Ordering::Relaxed),
        _ => 0,
    };
}

/// Snapshot the counters, rendered as Prometheus text-exposition lines.
pub(crate) fn render() -> String {
    let total = REQUESTS.load(Ordering::Relaxed);
    let c2 = RESP_2XX.load(Ordering::Relaxed);
    let c4 = RESP_4XX.load(Ordering::Relaxed);
    let c5 = RESP_5XX.load(Ordering::Relaxed);
    let dur = DURATION_MS_SUM.load(Ordering::Relaxed);
    format!(
        "# HELP summarybot_http_requests_total HTTP requests handled (excludes health/metrics).\n\
         # TYPE summarybot_http_requests_total counter\n\
         summarybot_http_requests_total {total}\n\
         # HELP summarybot_http_responses_total HTTP responses by status class.\n\
         # TYPE summarybot_http_responses_total counter\n\
         summarybot_http_responses_total{{class=\"2xx\"}} {c2}\n\
         summarybot_http_responses_total{{class=\"4xx\"}} {c4}\n\
         summarybot_http_responses_total{{class=\"5xx\"}} {c5}\n\
         # HELP summarybot_http_request_duration_ms_sum Cumulative request duration (ms).\n\
         # TYPE summarybot_http_request_duration_ms_sum counter\n\
         summarybot_http_request_duration_ms_sum {dur}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_by_class_and_renders() {
        // Counters are process-global; assert on deltas, not absolutes.
        let before = REQUESTS.load(Ordering::Relaxed);
        record(200, 5);
        record(404, 1);
        record(500, 9);
        assert_eq!(REQUESTS.load(Ordering::Relaxed), before + 3);
        let out = render();
        assert!(out.contains("summarybot_http_requests_total"));
        assert!(out.contains("class=\"4xx\""));
        assert!(out.contains("summarybot_http_request_duration_ms_sum"));
    }
}
