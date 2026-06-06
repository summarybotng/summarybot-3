//! Coverage-gap analysis for upload-only sources (PRD §2.3 WHA-016/017; ADR-121,
//! supersedes ref ADR-112).
//!
//! WhatsApp data arrives as partial, overlapping snapshots, so a chat is almost
//! never fully covered. This turns a set of imported date-ranges (plus the
//! chat's known creation time, from a detected `GroupCreated` event) into a
//! merged coverage picture with classified, **fillable** gaps — the data behind
//! the coverage timeline and the "ask members to export X" invitations. Pure:
//! the clock (`now`) is an input.

/// A covered interval in UTC unix seconds (`start <= end`), e.g. one import's
/// date range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: i64,
    pub end: i64,
}

impl Span {
    pub fn new(start: i64, end: i64) -> Self {
        Self { start, end }
    }
}

/// Why a period is missing (WHA-016).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapKind {
    /// Before the earliest import, back to the chat's creation — history that
    /// existed but predates anyone's export.
    BeforeJoin,
    /// Between two imports — an un-imported stretch in the middle.
    BetweenImports,
    /// After the latest import, up to now — messages that may have accrued since.
    AfterLast,
}

/// A classified gap. `can_fill` marks that data plausibly exists and someone
/// could contribute it (vs. a proven-empty period). The engine cannot prove
/// non-existence from imports alone, so emitted gaps are fillable; the field is
/// kept for API parity and future "exhausted" markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverageGap {
    pub start: i64,
    pub end: i64,
    pub kind: GapKind,
    pub can_fill: bool,
}

/// The merged coverage picture for one chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageReport {
    /// Earliest/latest covered instant, or `None` when there are no imports.
    pub earliest: Option<i64>,
    pub latest: Option<i64>,
    /// Total covered seconds after merging overlaps.
    pub covered_secs: i64,
    pub gaps: Vec<CoverageGap>,
}

/// Analyze coverage from import `spans`, the chat's `created_at` (if a
/// `GroupCreated` event was detected), the present `now`, and a `min_gap_secs`
/// floor below which gaps are ignored as noise (e.g. one day). Overlapping
/// imports are merged first, so re-uploads and partial overlaps don't distort
/// the picture.
pub fn analyze_coverage(
    spans: &[Span],
    created_at: Option<i64>,
    now: i64,
    min_gap_secs: i64,
) -> CoverageReport {
    let merged = merge_spans(spans);
    let Some(first) = merged.first().copied() else {
        // No imports: nothing to anchor a gap against; the caller knows the
        // chat is entirely un-imported.
        return CoverageReport {
            earliest: None,
            latest: None,
            covered_secs: 0,
            gaps: Vec::new(),
        };
    };
    let last = *merged.last().expect("non-empty");
    let covered_secs = merged.iter().map(|s| s.end - s.start).sum();

    let mut gaps = Vec::new();
    if let Some(created) = created_at {
        if first.start - created > min_gap_secs {
            gaps.push(CoverageGap {
                start: created,
                end: first.start,
                kind: GapKind::BeforeJoin,
                can_fill: true,
            });
        }
    }
    for w in merged.windows(2) {
        if w[1].start - w[0].end > min_gap_secs {
            gaps.push(CoverageGap {
                start: w[0].end,
                end: w[1].start,
                kind: GapKind::BetweenImports,
                can_fill: true,
            });
        }
    }
    if now - last.end > min_gap_secs {
        gaps.push(CoverageGap {
            start: last.end,
            end: now,
            kind: GapKind::AfterLast,
            can_fill: true,
        });
    }

    CoverageReport {
        earliest: Some(first.start),
        latest: Some(last.end),
        covered_secs,
        gaps,
    }
}

/// Sort and merge overlapping/touching spans (dropping inverted ones).
fn merge_spans(spans: &[Span]) -> Vec<Span> {
    let mut v: Vec<Span> = spans.iter().copied().filter(|s| s.end >= s.start).collect();
    v.sort_by_key(|s| s.start);
    let mut merged: Vec<Span> = Vec::new();
    for s in v {
        match merged.last_mut() {
            Some(last) if s.start <= last.end => last.end = last.end.max(s.end),
            _ => merged.push(s),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    #[test]
    fn no_imports_yields_empty_report() {
        let r = analyze_coverage(&[], Some(0), 100 * DAY, DAY);
        assert_eq!(r.earliest, None);
        assert!(r.gaps.is_empty());
        assert_eq!(r.covered_secs, 0);
    }

    #[test]
    fn single_full_span_no_gaps_when_current_and_no_creation() {
        // Latest import ends at `now`; no creation date → no before/after gap.
        let r = analyze_coverage(&[Span::new(10 * DAY, 20 * DAY)], None, 20 * DAY, DAY);
        assert_eq!(r.earliest, Some(10 * DAY));
        assert_eq!(r.latest, Some(20 * DAY));
        assert!(r.gaps.is_empty());
        assert_eq!(r.covered_secs, 10 * DAY);
    }

    #[test]
    fn overlapping_imports_merge() {
        let r = analyze_coverage(
            &[Span::new(0, 10 * DAY), Span::new(5 * DAY, 15 * DAY)],
            None,
            15 * DAY,
            DAY,
        );
        assert!(r.gaps.is_empty());
        assert_eq!(r.covered_secs, 15 * DAY); // merged, not 20
    }

    #[test]
    fn gap_between_two_imports() {
        let r = analyze_coverage(
            &[Span::new(0, 10 * DAY), Span::new(20 * DAY, 30 * DAY)],
            None,
            30 * DAY,
            DAY,
        );
        assert_eq!(r.gaps.len(), 1);
        assert_eq!(r.gaps[0].kind, GapKind::BetweenImports);
        assert_eq!((r.gaps[0].start, r.gaps[0].end), (10 * DAY, 20 * DAY));
    }

    #[test]
    fn before_join_gap_when_chat_predates_earliest_import() {
        // Chat created day 0, but earliest import starts day 100.
        let r = analyze_coverage(&[Span::new(100 * DAY, 110 * DAY)], Some(0), 110 * DAY, DAY);
        assert_eq!(r.gaps.len(), 1);
        assert_eq!(r.gaps[0].kind, GapKind::BeforeJoin);
        assert_eq!((r.gaps[0].start, r.gaps[0].end), (0, 100 * DAY));
        assert!(r.gaps[0].can_fill);
    }

    #[test]
    fn after_last_gap_when_latest_import_is_stale() {
        let r = analyze_coverage(&[Span::new(0, 10 * DAY)], None, 50 * DAY, DAY);
        assert_eq!(r.gaps.len(), 1);
        assert_eq!(r.gaps[0].kind, GapKind::AfterLast);
        assert_eq!((r.gaps[0].start, r.gaps[0].end), (10 * DAY, 50 * DAY));
    }

    #[test]
    fn gaps_below_threshold_are_ignored() {
        // 1-hour gap, threshold one day → ignored.
        let r = analyze_coverage(
            &[Span::new(0, 10 * DAY), Span::new(10 * DAY + 3600, 20 * DAY)],
            None,
            20 * DAY,
            DAY,
        );
        assert!(r.gaps.is_empty());
    }

    #[test]
    fn full_picture_before_between_and_after() {
        let r = analyze_coverage(
            &[
                Span::new(100 * DAY, 110 * DAY),
                Span::new(130 * DAY, 140 * DAY),
            ],
            Some(0),
            200 * DAY,
            DAY,
        );
        let kinds: Vec<GapKind> = r.gaps.iter().map(|g| g.kind).collect();
        assert_eq!(
            kinds,
            vec![
                GapKind::BeforeJoin,
                GapKind::BetweenImports,
                GapKind::AfterLast
            ]
        );
    }
}
