//! WhatsApp coverage orchestration (WHA-016/017; ADR-121) — ties stored import
//! spans to the pure `analyze_coverage` gap algorithm.
//!
//! WhatsApp is upload-only, so a chat is covered by a set of partial, overlapping
//! exports. This reads a chat's imports, anchors the back-history to the detected
//! group-creation instant (WHA-015, the earliest across imports), and returns the
//! merged coverage picture with classified fillable gaps — the data behind the
//! coverage timeline and "ask members to export X" prompts.

use domain::{analyze_coverage, CoverageReport, Span, WorkspaceId};
use repository::{ChatSummary, WhatsAppRepository};

/// Gaps shorter than this are ignored as noise (a chat is never gap-free to the
/// second). One day.
const MIN_GAP_SECS: i64 = 86_400;

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
