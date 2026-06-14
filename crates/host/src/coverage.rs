//! WhatsApp coverage orchestration (WHA-016/017; ADR-121) — ties stored import
//! spans to the pure `analyze_coverage` gap algorithm.
//!
//! WhatsApp is upload-only, so a chat is covered by a set of partial, overlapping
//! exports. This reads a chat's imports, anchors the back-history to the detected
//! group-creation instant (WHA-015, the earliest across imports), and returns the
//! merged coverage picture with classified fillable gaps — the data behind the
//! coverage timeline and "ask members to export X" prompts.

use domain::{analyze_coverage, CoverageReport, Span, UserId, WorkspaceId};
use repository::{ChatSummary, ImportInvitation, NewImportInvitation, WhatsAppRepository};

/// Gaps shorter than this are ignored as noise (a chat is never gap-free to the
/// second). One day.
const MIN_GAP_SECS: i64 = 86_400;

/// One member's contribution to a chat (WHA-018): which uploader supplied how
/// much, over what span. Aggregated from the chat's import records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contribution {
    pub uploader: String,
    pub import_count: i64,
    pub message_count: i64,
    /// Earliest `date_start` across this uploader's imports.
    pub earliest: i64,
    /// Latest `date_end` across this uploader's imports.
    pub latest: i64,
}

/// A chat plus its coverage picture, for the per-workspace overview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatCoverage {
    pub summary: ChatSummary,
    pub report: CoverageReport,
}

/// Coverage for one chat (WHA-016): merge its import spans, anchor `before_join`
/// to the earliest detected group-creation, classify gaps up to `now`.
pub fn coverage_for<R: WhatsAppRepository>(
    repo: &R,
    workspace: &WorkspaceId,
    chat: &domain::ChannelId,
    now: i64,
) -> anyhow::Result<CoverageReport> {
    let imports = repo.list_imports(workspace, chat)?;
    let spans: Vec<Span> = imports
        .iter()
        .map(|i| Span::new(i.date_start, i.date_end))
        .collect();
    // The chat's creation: earliest creation instant detected across imports.
    let created_at = imports.iter().filter_map(|i| i.group_created_at).min();
    Ok(analyze_coverage(&spans, created_at, now, MIN_GAP_SECS))
}

/// Per-contributor rollup for a chat (WHA-018): who has supplied which date
/// ranges, so the team can see — and credit — who filled what. Ordered by
/// earliest contribution.
pub fn contributors_for<R: WhatsAppRepository>(
    repo: &R,
    workspace: &WorkspaceId,
    chat: &domain::ChannelId,
) -> anyhow::Result<Vec<Contribution>> {
    use std::collections::BTreeMap;
    let mut by_user: BTreeMap<String, Contribution> = BTreeMap::new();
    for imp in repo.list_imports(workspace, chat)? {
        let e = by_user.entry(imp.uploader.clone()).or_insert(Contribution {
            uploader: imp.uploader.clone(),
            import_count: 0,
            message_count: 0,
            earliest: imp.date_start,
            latest: imp.date_end,
        });
        e.import_count += 1;
        e.message_count += imp.message_count;
        e.earliest = e.earliest.min(imp.date_start);
        e.latest = e.latest.max(imp.date_end);
    }
    let mut out: Vec<Contribution> = by_user.into_values().collect();
    out.sort_by_key(|c| (c.earliest, c.uploader.clone()));
    Ok(out)
}

/// Still-fillable gap seconds overlapping `[start, end]` in a coverage report —
/// how much of an invitation's range remains uncovered (WHA-019 fulfillment).
fn unfilled_secs_in(report: &CoverageReport, start: i64, end: i64) -> i64 {
    report
        .gaps
        .iter()
        .filter(|g| g.can_fill)
        .map(|g| (g.end.min(end) - g.start.max(start)).max(0))
        .sum()
}

/// Open a scoped import invitation against a gap (WHA-019). Returns the id.
#[allow(clippy::too_many_arguments)]
pub fn open_invitation<R: WhatsAppRepository>(
    repo: &R,
    workspace: &WorkspaceId,
    chat: &domain::ChannelId,
    range_start: i64,
    range_end: i64,
    kind: &str,
    note: &str,
    created_by: &UserId,
    now: i64,
) -> anyhow::Result<String> {
    let id = format!("inv_{now}_{}_{}", range_start, range_end);
    repo.create_invitation(&NewImportInvitation {
        id: &id,
        workspace_id: workspace,
        chat_id: chat,
        range_start,
        range_end,
        kind,
        note,
        created_by,
        created_at: now,
    })?;
    Ok(id)
}

/// Mark an invitation cancelled (WHA-019). Returns `false` if no such open row.
pub fn cancel_invitation<R: WhatsAppRepository>(
    repo: &R,
    workspace: &WorkspaceId,
    id: &str,
) -> anyhow::Result<bool> {
    repo.set_invitation_status(workspace, id, "cancelled", None, None)
}

/// Reconcile a chat's open invitations against current coverage after an import
/// (WHA-019): any open invitation whose range is now substantially covered (less
/// than [`MIN_GAP_SECS`] of fillable gap remains in it) is marked `fulfilled`,
/// credited to `fulfilled_by` (the uploader who just contributed). Best-effort —
/// returns how many were closed.
pub fn reconcile_invitations<R: WhatsAppRepository>(
    repo: &R,
    workspace: &WorkspaceId,
    chat: &domain::ChannelId,
    fulfilled_by: &UserId,
    now: i64,
) -> anyhow::Result<usize> {
    let report = coverage_for(repo, workspace, chat, now)?;
    let mut closed = 0;
    for inv in repo.list_invitations(workspace, chat)? {
        if inv.status != "open" {
            continue;
        }
        if unfilled_secs_in(&report, inv.range_start, inv.range_end) < MIN_GAP_SECS {
            repo.set_invitation_status(
                workspace,
                &inv.id,
                "fulfilled",
                Some(fulfilled_by.as_str()),
                Some(now),
            )?;
            closed += 1;
        }
    }
    Ok(closed)
}

/// A chat's invitations, newest first (WHA-019).
pub fn list_invitations<R: WhatsAppRepository>(
    repo: &R,
    workspace: &WorkspaceId,
    chat: &domain::ChannelId,
) -> anyhow::Result<Vec<ImportInvitation>> {
    repo.list_invitations(workspace, chat)
}

/// Coverage for every chat in the workspace (WHA-017 overview).
pub fn workspace_coverage<R: WhatsAppRepository>(
    repo: &R,
    workspace: &WorkspaceId,
    now: i64,
) -> anyhow::Result<Vec<ChatCoverage>> {
    let chats = repo.list_chats(workspace)?;
    let mut out = Vec::with_capacity(chats.len());
    for summary in chats {
        let chat = domain::ChannelId::parse(&summary.chat_id)?;
        let report = coverage_for(repo, workspace, &chat, now)?;
        out.push(ChatCoverage { summary, report });
    }
    Ok(out)
}

// ----- General content coverage (ADR-133) -------------------------------------
//
// Generalizes the WhatsApp picture to *any* source: how much of each channel's
// stored history is covered by summaries. Reuses the pure `analyze_coverage`
// gap algorithm, feeding it each channel's summary windows. A workspace-wide
// summary (all-channels / category schedule, no single `channel_id`) covers
// every channel, so its span is applied to each.

/// One channel's coverage row for the dashboard (ADR-133).
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelContentCoverage {
    pub channel_id: String,
    pub earliest_content: i64,
    pub latest_content: i64,
    pub message_count: i64,
    pub summary_count: i64,
    /// Covered seconds (after merging overlaps) / content span, as a percent.
    /// Can exceed 100 when summary windows overlap (matches v2).
    pub coverage_percent: f64,
    pub gap_count: i64,
    pub gaps: Vec<domain::CoverageGap>,
}

/// Workspace-wide content coverage rollup (ADR-133).
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceContentCoverage {
    pub total_coverage_percent: f64,
    pub total_gaps: i64,
    pub total_channels: i64,
    pub covered_channels: i64,
    pub total_summaries: i64,
    pub earliest_content: Option<i64>,
    pub latest_content: Option<i64>,
    pub channels: Vec<ChannelContentCoverage>,
}

/// Compute coverage for every channel that has stored messages (ADR-133). Gaps
/// for a channel are measured within its own content range (anchor = earliest
/// message, horizon = latest message), so an idle channel isn't penalized for
/// time after its last message.
pub fn content_coverage<R: repository::CoverageRepository>(
    repo: &R,
    workspace: &WorkspaceId,
) -> anyhow::Result<WorkspaceContentCoverage> {
    use std::collections::HashMap;

    let content = repo.channel_content(workspace)?;
    let spans = repo.summary_spans(workspace)?;

    let workspace_wide: Vec<Span> = spans
        .iter()
        .filter(|s| s.channel_id.is_none())
        .map(|s| Span::new(s.start, s.end))
        .collect();
    let mut by_channel: HashMap<String, Vec<Span>> = HashMap::new();
    for s in &spans {
        if let Some(c) = &s.channel_id {
            by_channel.entry(c.clone()).or_default().push(Span::new(s.start, s.end));
        }
    }

    let mut channels = Vec::with_capacity(content.len());
    let (mut sum_covered, mut sum_content, mut total_gaps, mut covered_channels) = (0i64, 0i64, 0i64, 0i64);
    for c in &content {
        let mut chan_spans = by_channel.get(&c.channel_id).cloned().unwrap_or_default();
        chan_spans.extend(workspace_wide.iter().copied());
        // Horizon = latest message; anchor = earliest message.
        let report = analyze_coverage(&chan_spans, Some(c.earliest), c.latest, MIN_GAP_SECS);
        let content_secs = (c.latest - c.earliest).max(1);
        let pct = report.covered_secs as f64 / content_secs as f64 * 100.0;
        let summary_count = chan_spans.len() as i64;
        if summary_count > 0 {
            covered_channels += 1;
        }
        sum_covered += report.covered_secs;
        sum_content += content_secs;
        total_gaps += report.gaps.len() as i64;
        channels.push(ChannelContentCoverage {
            channel_id: c.channel_id.clone(),
            earliest_content: c.earliest,
            latest_content: c.latest,
            message_count: c.message_count,
            summary_count,
            coverage_percent: pct,
            gap_count: report.gaps.len() as i64,
            gaps: report.gaps,
        });
    }
    // Most-covered first, then by id, for a stable, useful default order.
    channels.sort_by(|a, b| {
        b.coverage_percent
            .partial_cmp(&a.coverage_percent)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.channel_id.cmp(&b.channel_id))
    });

    let total_pct = if sum_content > 0 {
        sum_covered as f64 / sum_content as f64 * 100.0
    } else {
        0.0
    };
    Ok(WorkspaceContentCoverage {
        total_coverage_percent: total_pct,
        total_gaps,
        total_channels: content.len() as i64,
        covered_channels,
        total_summaries: spans.len() as i64,
        earliest_content: content.iter().map(|c| c.earliest).min(),
        latest_content: content.iter().map(|c| c.latest).max(),
        channels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::GapKind;
    use repository::{ImportRecord, SqliteRepository};

    fn ws() -> WorkspaceId {
        WorkspaceId::parse("ws-1").unwrap()
    }
    fn chat() -> domain::ChannelId {
        domain::ChannelId::parse("c1").unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn rec<'a>(
        id: &'a str,
        hash: &'a str,
        ws: &'a WorkspaceId,
        c: &'a domain::ChannelId,
        up: &'a domain::UserId,
        start: i64,
        end: i64,
        created: Option<i64>,
    ) -> ImportRecord<'a> {
        ImportRecord {
            id,
            workspace_id: ws,
            chat_id: c,
            file_hash: hash,
            uploader: up,
            imported_at: end,
            format: "ios",
            message_count: 5,
            date_start: start,
            date_end: end,
            group_created_at: created,
        }
    }

    const DAY: i64 = 86_400;

    #[test]
    fn classifies_before_between_and_after_gaps() {
        let repo = SqliteRepository::in_memory().unwrap();
        let (ws, c) = (ws(), chat());
        let up = domain::UserId::parse("u1").unwrap();
        // Group created at day 0; imports cover days 10–20 and 40–50; now is day 80.
        repo.record_import(&rec("i1", "h1", &ws, &c, &up, 10 * DAY, 20 * DAY, Some(0)))
            .unwrap();
        repo.record_import(&rec("i2", "h2", &ws, &c, &up, 40 * DAY, 50 * DAY, None))
            .unwrap();

        let r = coverage_for(&repo, &ws, &c, 80 * DAY).unwrap();
        assert_eq!(r.earliest, Some(10 * DAY));
        assert_eq!(r.latest, Some(50 * DAY));
        let kinds: Vec<_> = r.gaps.iter().map(|g| g.kind).collect();
        assert!(kinds.contains(&GapKind::BeforeJoin)); // 0 → 10d (created → first import)
        assert!(kinds.contains(&GapKind::BetweenImports)); // 20d → 40d
        assert!(kinds.contains(&GapKind::AfterLast)); // 50d → 80d (now)
    }

    #[test]
    fn contributors_aggregate_per_uploader() {
        let repo = SqliteRepository::in_memory().unwrap();
        let (ws, c) = (ws(), chat());
        let alice = domain::UserId::parse("alice").unwrap();
        let bob = domain::UserId::parse("bob").unwrap();
        // Alice: two imports (10–20d, 40–50d); Bob: one (25–30d).
        repo.record_import(&rec("i1", "h1", &ws, &c, &alice, 10 * DAY, 20 * DAY, None))
            .unwrap();
        repo.record_import(&rec("i2", "h2", &ws, &c, &alice, 40 * DAY, 50 * DAY, None))
            .unwrap();
        repo.record_import(&rec("i3", "h3", &ws, &c, &bob, 25 * DAY, 30 * DAY, None))
            .unwrap();

        let contribs = contributors_for(&repo, &ws, &c).unwrap();
        assert_eq!(contribs.len(), 2);
        // Ordered by earliest contribution → Alice (10d) before Bob (25d).
        assert_eq!(contribs[0].uploader, "alice");
        assert_eq!(contribs[0].import_count, 2);
        assert_eq!(contribs[0].earliest, 10 * DAY);
        assert_eq!(contribs[0].latest, 50 * DAY);
        assert_eq!(contribs[1].uploader, "bob");
        assert_eq!(contribs[1].import_count, 1);
    }

    #[test]
    fn invitation_is_fulfilled_when_a_covering_import_lands() {
        let repo = SqliteRepository::in_memory().unwrap();
        let (ws, c) = (ws(), chat());
        let asker = domain::UserId::parse("asker").unwrap();
        let helper = domain::UserId::parse("helper").unwrap();
        // One import covers 40–50d; gap 20d→40d is open. Existing import 10–20d.
        repo.record_import(&rec("i1", "h1", &ws, &c, &asker, 10 * DAY, 20 * DAY, None))
            .unwrap();
        repo.record_import(&rec("i2", "h2", &ws, &c, &asker, 40 * DAY, 50 * DAY, None))
            .unwrap();

        // Ask for the between-imports gap.
        let id = open_invitation(
            &repo,
            &ws,
            &c,
            20 * DAY,
            40 * DAY,
            "between_imports",
            "please export late Jan",
            &asker,
            60 * DAY,
        )
        .unwrap();
        assert_eq!(list_invitations(&repo, &ws, &c).unwrap()[0].status, "open");

        // A helper imports the full missing 20–40d range → reconcile closes the ask.
        repo.record_import(&rec("i3", "h3", &ws, &c, &helper, 20 * DAY, 40 * DAY, None))
            .unwrap();
        let closed = reconcile_invitations(&repo, &ws, &c, &helper, 61 * DAY).unwrap();
        assert_eq!(closed, 1);
        let inv = repo.get_invitation(&ws, &id).unwrap().unwrap();
        assert_eq!(inv.status, "fulfilled");
        assert_eq!(inv.fulfilled_by.as_deref(), Some("helper"));

        // Reconciling again is a no-op (already closed).
        assert_eq!(reconcile_invitations(&repo, &ws, &c, &helper, 62 * DAY).unwrap(), 0);
    }

    #[test]
    fn open_invitation_survives_until_cancelled_when_gap_remains() {
        let repo = SqliteRepository::in_memory().unwrap();
        let (ws, c) = (ws(), chat());
        let asker = domain::UserId::parse("asker").unwrap();
        repo.record_import(&rec("i1", "h1", &ws, &c, &asker, 10 * DAY, 20 * DAY, None))
            .unwrap();
        let id = open_invitation(
            &repo, &ws, &c, 30 * DAY, 60 * DAY, "after_last", "export recent", &asker, 70 * DAY,
        )
        .unwrap();
        // No covering import → still open after reconcile.
        assert_eq!(reconcile_invitations(&repo, &ws, &c, &asker, 71 * DAY).unwrap(), 0);
        assert_eq!(repo.get_invitation(&ws, &id).unwrap().unwrap().status, "open");
        // Cancel closes it.
        assert!(cancel_invitation(&repo, &ws, &id).unwrap());
        assert_eq!(repo.get_invitation(&ws, &id).unwrap().unwrap().status, "cancelled");
    }

    #[test]
    fn content_coverage_measures_per_channel_gaps() {
        use domain::summarize::{ExtractedSummary, SummaryUsage};
        use domain::{NormalizedMessage, Platform};
        use repository::{StructuredSummaryRepository, SummaryRecord};

        let repo = SqliteRepository::in_memory().unwrap();
        let ws = ws();
        let c = chat();
        // Messages spanning days 0..10 in channel c1.
        for (i, day) in [0i64, 5, 10].iter().enumerate() {
            repo.save_message(
                &ws,
                &NormalizedMessage {
                    id: domain::MessageId::parse(format!("m{i}")).unwrap(),
                    platform: Platform::WhatsApp,
                    channel_id: c.clone(),
                    author_id: "a".into(),
                    author_name: "Alice".into(),
                    content: "we shipped the release and planned the roadmap".into(),
                    timestamp: day * DAY,
                    is_system: false,
                    reply_to: None,
                    attachments: vec![],
                },
            )
            .unwrap();
        }
        // One summary covering only the first half [0, 5*DAY] → ~50% + a gap.
        let rec = SummaryRecord {
            id: "s1".into(),
            channel_id: Some(c.clone()),
            model: "demo".into(),
            cost_micros: 0,
            degraded: false,
            created_at: 5 * DAY,
            pinned: false,
            archived: false,
            tags: vec![],
            coherence_score: None,
            usage: SummaryUsage::default(),
            period_start: 0,
            period_end: 5 * DAY,
            summary: ExtractedSummary {
                text: "x".into(),
                key_points: vec![],
                action_items: vec![],
                technical_terms: vec![],
                participants: vec![],
                citations: vec![],
            },
        };
        repo.save_record(&ws, &rec).unwrap();

        let cov = content_coverage(&repo, &ws).unwrap();
        assert_eq!(cov.total_channels, 1);
        assert_eq!(cov.covered_channels, 1);
        assert_eq!(cov.channels.len(), 1);
        let ch = &cov.channels[0];
        assert_eq!(ch.channel_id, "c1");
        // Covered 5 of 10 days → ~50%.
        assert!((ch.coverage_percent - 50.0).abs() < 1.0, "got {}", ch.coverage_percent);
        // The uncovered second half is a gap (after the last summary).
        assert_eq!(ch.gap_count, 1);
    }

    #[test]
    fn workspace_overview_lists_each_chat() {
        let repo = SqliteRepository::in_memory().unwrap();
        let (ws, c) = (ws(), chat());
        let up = domain::UserId::parse("u1").unwrap();
        repo.record_import(&rec("i1", "h1", &ws, &c, &up, 10 * DAY, 20 * DAY, None))
            .unwrap();
        let cov = workspace_coverage(&repo, &ws, 30 * DAY).unwrap();
        assert_eq!(cov.len(), 1);
        assert_eq!(cov[0].summary.chat_id, "c1");
        assert_eq!(cov[0].summary.import_count, 1);
        assert_eq!(cov[0].report.latest, Some(20 * DAY));
    }
}
