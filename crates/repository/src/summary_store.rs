//! Structured summary storage (PRD §4, §5.1 — the always-on dashboard sink).
//!
//! Persists the full [`ExtractedSummary`] the pipeline produces, plus its model/
//! cost/degraded provenance. Lists (key points, technical terms, participants)
//! are newline-joined columns; action items and grounded citations are child
//! tables (citations keep the message-id link). This is the canonical store the
//! dashboard reads and other delivery destinations format from.

use crate::SqliteRepository;
use anyhow::Result;
use domain::summarize::{ActionItem, ExtractedSummary, ResolvedCitation};
use domain::{ChannelId, MessageId, WorkspaceId};
use rusqlite::types::ToSql;
use rusqlite::{params, OptionalExtension};

/// A stored summary with its provenance and management flags (§5.1).
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryRecord {
    pub id: String,
    /// The channel/scope this summary covered, if any.
    pub channel_id: Option<ChannelId>,
    pub model: String,
    pub cost_micros: i64,
    pub degraded: bool,
    pub created_at: i64,
    pub pinned: bool,
    pub archived: bool,
    pub tags: Vec<String>,
    pub summary: ExtractedSummary,
    /// Coherence-gate grounded score in `[0,1]` (COH-001); `None` if unassessed.
    pub coherence_score: Option<f32>,
}

/// Filter + pagination for a summary listing (PRD §5.1: DSH-002/004/005). All
/// filters are optional and combine with AND; text/participant/tag use
/// case-insensitive substring (`LIKE`) matching — a simple first cut. The
/// richer FTS5 dual-index is a Phase-7 concern (brief open Q#7), deliberately
/// not committed to here.
#[derive(Debug, Clone)]
pub struct SummaryQuery {
    /// Include archived summaries (DSH-008); excluded by default.
    pub include_archived: bool,
    /// Substring match over the summary text and key points (DSH-004).
    pub text: Option<String>,
    /// Substring match over the participant list (DSH-005).
    pub participant: Option<String>,
    /// Substring match over the tag list (DSH-009).
    pub tag: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

impl Default for SummaryQuery {
    fn default() -> Self {
        Self {
            include_archived: false,
            text: None,
            participant: None,
            tag: None,
            limit: 50,
            offset: 0,
        }
    }
}

/// Per-model spend rollup over a workspace's stored summaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpend {
    pub model: String,
    pub count: i64,
    pub cost_micros: i64,
}

/// A workspace's summarization spend, aggregated from its stored summaries —
/// what the cost dashboard shows (no new storage; summaries already carry
/// `cost_micros` + `model`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpendBreakdown {
    pub total_micros: i64,
    pub summary_count: i64,
    /// Spend since `now - recent_window` (caller passes the cutoff); `0` if none.
    pub recent_micros: i64,
    pub by_model: Vec<ModelSpend>,
}

/// Storage boundary for structured summaries (the dashboard sink + §5.1 mgmt).
pub trait StructuredSummaryRepository {
    fn save_record(&self, workspace: &WorkspaceId, record: &SummaryRecord) -> Result<()>;
    fn get_record(&self, workspace: &WorkspaceId, id: &str) -> Result<Option<SummaryRecord>>;
    /// List a workspace's summaries, newest first; archived ones are excluded
    /// unless `include_archived`. `limit` caps the result.
    fn list_records(
        &self,
        workspace: &WorkspaceId,
        include_archived: bool,
        limit: u32,
    ) -> Result<Vec<SummaryRecord>>;
    /// Filtered + paginated listing (DSH-002/004/005). Same ordering as
    /// [`StructuredSummaryRepository::list_records`] (pinned first, then newest).
    fn search_records(
        &self,
        workspace: &WorkspaceId,
        query: &SummaryQuery,
    ) -> Result<Vec<SummaryRecord>>;
    /// Pin/unpin (§5.1). Returns whether a row changed.
    fn set_pinned(&self, workspace: &WorkspaceId, id: &str, pinned: bool) -> Result<bool>;
    /// Archive/unarchive (§5.1).
    fn set_archived(&self, workspace: &WorkspaceId, id: &str, archived: bool) -> Result<bool>;
    /// Replace the tag set (§5.1).
    fn set_tags(&self, workspace: &WorkspaceId, id: &str, tags: &[String]) -> Result<bool>;
    /// Hard-delete a summary and its child rows (DSH-013), tenant-scoped. Returns
    /// whether a row was removed.
    fn delete_record(&self, workspace: &WorkspaceId, id: &str) -> Result<bool>;
    /// Aggregate the workspace's summarization spend (cost analytics). `since` is
    /// the cutoff (unix secs) for the `recent_micros` window.
    fn workspace_spend(&self, workspace: &WorkspaceId, since: i64) -> Result<SpendBreakdown>;
}

/// Join a string list into one column (entries can't contain newlines).
fn join(list: &[String]) -> String {
    list.join("\n")
}

/// Inverse of [`join`]; drops empties so a blank column yields `[]`.
fn split(s: &str) -> Vec<String> {
    s.lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

impl StructuredSummaryRepository for SqliteRepository {
    fn save_record(&self, workspace: &WorkspaceId, record: &SummaryRecord) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO summary_records
               (id, workspace_id, channel_id, model, cost_micros, degraded, created_at,
                text, key_points, technical_terms, participants, pinned, archived, tags,
                coherence_score)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![
                record.id,
                workspace.as_str(),
                record.channel_id.as_ref().map(|c| c.as_str()),
                record.model,
                record.cost_micros,
                record.degraded,
                record.created_at,
                record.summary.text,
                join(&record.summary.key_points),
                join(&record.summary.technical_terms),
                join(&record.summary.participants),
                record.pinned,
                record.archived,
                join(&record.tags),
                record.coherence_score,
            ],
        )?;
        for (i, a) in record.summary.action_items.iter().enumerate() {
            tx.execute(
                "INSERT INTO summary_action_items (summary_id, idx, text, assignee)
                 VALUES (?1,?2,?3,?4)",
                params![record.id, i as i64, a.text, a.assignee],
            )?;
        }
        for (i, c) in record.summary.citations.iter().enumerate() {
            tx.execute(
                "INSERT INTO summary_citations (summary_id, idx, message_id, quote)
                 VALUES (?1,?2,?3,?4)",
                params![record.id, i as i64, c.message_id.as_str(), c.quote],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn get_record(&self, workspace: &WorkspaceId, id: &str) -> Result<Option<SummaryRecord>> {
        let main = self
            .conn
            .query_row(
                "SELECT channel_id, model, cost_micros, degraded, created_at,
                        text, key_points, technical_terms, participants, pinned, archived, tags,
                        coherence_score
                 FROM summary_records WHERE id = ?1 AND workspace_id = ?2",
                params![id, workspace.as_str()],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, bool>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, bool>(9)?,
                        row.get::<_, bool>(10)?,
                        row.get::<_, String>(11)?,
                        row.get::<_, Option<f32>>(12)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            channel,
            model,
            cost,
            degraded,
            created,
            text,
            kp,
            tt,
            parts,
            pinned,
            archived,
            tags,
            coherence_score,
        )) = main
        else {
            return Ok(None);
        };

        let mut ai_stmt = self.conn.prepare(
            "SELECT text, assignee FROM summary_action_items WHERE summary_id = ?1 ORDER BY idx",
        )?;
        let action_items = ai_stmt
            .query_map(params![id], |row| {
                Ok(ActionItem {
                    text: row.get(0)?,
                    assignee: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut c_stmt = self.conn.prepare(
            "SELECT message_id, quote FROM summary_citations WHERE summary_id = ?1 ORDER BY idx",
        )?;
        let citations = c_stmt
            .query_map(params![id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|(mid, quote)| {
                Ok(ResolvedCitation {
                    message_id: MessageId::parse(mid).map_err(anyhow::Error::new)?,
                    quote,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let channel_id = channel
            .map(ChannelId::parse)
            .transpose()
            .map_err(anyhow::Error::new)?;
        Ok(Some(SummaryRecord {
            id: id.to_string(),
            channel_id,
            model,
            cost_micros: cost,
            degraded,
            created_at: created,
            pinned,
            archived,
            tags: split(&tags),
            summary: ExtractedSummary {
                text,
                key_points: split(&kp),
                technical_terms: split(&tt),
                participants: split(&parts),
                action_items,
                citations,
            },
            coherence_score,
        }))
    }

    fn list_records(
        &self,
        workspace: &WorkspaceId,
        include_archived: bool,
        limit: u32,
    ) -> Result<Vec<SummaryRecord>> {
        // Pinned first, then newest; archived excluded unless asked. (List
        // reuses get_record per id — acceptable here; a projection can replace
        // it if the N+1 ever matters.)
        let sql = "SELECT id FROM summary_records
                   WHERE workspace_id = ?1 AND (?2 OR archived = 0)
                   ORDER BY pinned DESC, created_at DESC
                   LIMIT ?3";
        let mut stmt = self.conn.prepare(sql)?;
        let ids: Vec<String> = stmt
            .query_map(
                params![workspace.as_str(), include_archived, limit],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<_>>()?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(rec) = self.get_record(workspace, &id)? {
                out.push(rec);
            }
        }
        Ok(out)
    }

    fn search_records(
        &self,
        workspace: &WorkspaceId,
        query: &SummaryQuery,
    ) -> Result<Vec<SummaryRecord>> {
        // Build the WHERE dynamically; bind every value (no string interpolation
        // of user input — only `?n` placeholders). LIKE patterns are lowercased
        // and matched against lowercased columns for case-insensitivity.
        let ws = workspace.as_str().to_string();
        let like = |s: &str| format!("%{}%", s.to_lowercase());
        let text_pat = query.text.as_deref().map(like);
        let part_pat = query.participant.as_deref().map(like);
        let tag_pat = query.tag.as_deref().map(like);
        let limit = query.limit;
        let offset = query.offset;

        let mut sql = String::from(
            "SELECT id FROM summary_records WHERE workspace_id = ?1 AND (?2 OR archived = 0)",
        );
        let mut binds: Vec<&dyn ToSql> = vec![&ws, &query.include_archived];
        let mut n = 3;
        if let Some(p) = &text_pat {
            // ?n is referenced twice (text OR key_points) — one bound value.
            sql.push_str(&format!(
                " AND (lower(text) LIKE ?{n} OR lower(key_points) LIKE ?{n})"
            ));
            binds.push(p);
            n += 1;
        }
        if let Some(p) = &part_pat {
            sql.push_str(&format!(" AND lower(participants) LIKE ?{n}"));
            binds.push(p);
            n += 1;
        }
        if let Some(p) = &tag_pat {
            sql.push_str(&format!(" AND lower(tags) LIKE ?{n}"));
            binds.push(p);
            n += 1;
        }
        sql.push_str(&format!(
            " ORDER BY pinned DESC, created_at DESC LIMIT ?{} OFFSET ?{}",
            n,
            n + 1
        ));
        binds.push(&limit);
        binds.push(&offset);

        let mut stmt = self.conn.prepare(&sql)?;
        let ids: Vec<String> = stmt
            .query_map(binds.as_slice(), |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(rec) = self.get_record(workspace, &id)? {
                out.push(rec);
            }
        }
        Ok(out)
    }

    fn set_pinned(&self, workspace: &WorkspaceId, id: &str, pinned: bool) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE summary_records SET pinned = ?3 WHERE id = ?1 AND workspace_id = ?2",
            params![id, workspace.as_str(), pinned],
        )?;
        Ok(n > 0)
    }

    fn set_archived(&self, workspace: &WorkspaceId, id: &str, archived: bool) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE summary_records SET archived = ?3 WHERE id = ?1 AND workspace_id = ?2",
            params![id, workspace.as_str(), archived],
        )?;
        Ok(n > 0)
    }

    fn set_tags(&self, workspace: &WorkspaceId, id: &str, tags: &[String]) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE summary_records SET tags = ?3 WHERE id = ?1 AND workspace_id = ?2",
            params![id, workspace.as_str(), join(tags)],
        )?;
        Ok(n > 0)
    }

    fn delete_record(&self, workspace: &WorkspaceId, id: &str) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        // Scope the delete to the workspace (TEN-007); only purge child rows if
        // the parent actually belonged here.
        let n = tx.execute(
            "DELETE FROM summary_records WHERE id = ?1 AND workspace_id = ?2",
            params![id, workspace.as_str()],
        )?;
        if n > 0 {
            tx.execute(
                "DELETE FROM summary_action_items WHERE summary_id = ?1",
                params![id],
            )?;
            tx.execute(
                "DELETE FROM summary_citations WHERE summary_id = ?1",
                params![id],
            )?;
        }
        tx.commit()?;
        Ok(n > 0)
    }

    fn workspace_spend(&self, workspace: &WorkspaceId, since: i64) -> Result<SpendBreakdown> {
        let (total_micros, summary_count): (i64, i64) = self.conn.query_row(
            "SELECT COALESCE(SUM(cost_micros), 0), COUNT(*)
             FROM summary_records WHERE workspace_id = ?1",
            params![workspace.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let recent_micros: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(cost_micros), 0)
             FROM summary_records WHERE workspace_id = ?1 AND created_at >= ?2",
            params![workspace.as_str(), since],
            |row| row.get(0),
        )?;
        let mut stmt = self.conn.prepare(
            "SELECT model, COUNT(*), COALESCE(SUM(cost_micros), 0)
             FROM summary_records WHERE workspace_id = ?1
             GROUP BY model ORDER BY SUM(cost_micros) DESC",
        )?;
        let by_model = stmt
            .query_map(params![workspace.as_str()], |row| {
                Ok(ModelSpend {
                    model: row.get(0)?,
                    count: row.get(1)?,
                    cost_micros: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(SpendBreakdown {
            total_micros,
            summary_count,
            recent_micros,
            by_model,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> SqliteRepository {
        SqliteRepository::in_memory().unwrap()
    }
    fn ws() -> WorkspaceId {
        WorkspaceId::parse("ws-1").unwrap()
    }

    fn record() -> SummaryRecord {
        SummaryRecord {
            id: "sum_1".into(),
            channel_id: Some(ChannelId::parse("c1").unwrap()),
            model: "sonnet".into(),
            cost_micros: 12_345,
            degraded: true,
            created_at: 1_700_000_000,
            pinned: false,
            archived: false,
            tags: vec!["release".into()],
            coherence_score: Some(0.8),
            summary: ExtractedSummary {
                text: "We shipped.".into(),
                key_points: vec!["Launched Friday".into(), "Changelog done".into()],
                technical_terms: vec!["WASM".into()],
                participants: vec!["Alice".into(), "Bob".into()],
                action_items: vec![
                    ActionItem {
                        text: "notify users".into(),
                        assignee: Some("Alice".into()),
                    },
                    ActionItem {
                        text: "monitor errors".into(),
                        assignee: None,
                    },
                ],
                citations: vec![ResolvedCitation {
                    message_id: MessageId::parse("m0").unwrap(),
                    quote: Some("ship it".into()),
                }],
            },
        }
    }

    #[test]
    fn workspace_spend_aggregates_by_model_and_window() {
        let repo = repo();
        let mut a = record();
        a.id = "s_a".into();
        a.model = "sonnet".into();
        a.cost_micros = 1_000;
        a.created_at = 1_000;
        let mut b = record();
        b.id = "s_b".into();
        b.model = "sonnet".into();
        b.cost_micros = 2_000;
        b.created_at = 5_000;
        let mut c = record();
        c.id = "s_c".into();
        c.model = "opus".into();
        c.cost_micros = 9_000;
        c.created_at = 5_000;
        for r in [&a, &b, &c] {
            repo.save_record(&ws(), r).unwrap();
        }

        let spend = repo.workspace_spend(&ws(), 3_000).unwrap();
        assert_eq!(spend.total_micros, 12_000);
        assert_eq!(spend.summary_count, 3);
        // Only b + c are at/after the cutoff 3_000.
        assert_eq!(spend.recent_micros, 11_000);
        // Ordered by spend desc: opus (9_000) before sonnet (3_000).
        assert_eq!(spend.by_model[0].model, "opus");
        assert_eq!(spend.by_model[0].cost_micros, 9_000);
        assert_eq!(spend.by_model[1].model, "sonnet");
        assert_eq!(spend.by_model[1].count, 2);
        assert_eq!(spend.by_model[1].cost_micros, 3_000);

        // Scoped: a different workspace sees nothing.
        let other = repo
            .workspace_spend(&WorkspaceId::parse("ws-2").unwrap(), 0)
            .unwrap();
        assert_eq!(other, SpendBreakdown::default());
    }

    #[test]
    fn save_then_get_round_trips_full_structure() {
        let repo = repo();
        let rec = record();
        repo.save_record(&ws(), &rec).unwrap();
        let got = repo.get_record(&ws(), "sum_1").unwrap().unwrap();
        assert_eq!(got, rec);
    }

    #[test]
    fn get_is_workspace_scoped() {
        let repo = repo();
        repo.save_record(&ws(), &record()).unwrap();
        let other = WorkspaceId::parse("ws-other").unwrap();
        assert!(repo.get_record(&other, "sum_1").unwrap().is_none());
    }

    #[test]
    fn missing_record_is_none() {
        let repo = repo();
        assert!(repo.get_record(&ws(), "nope").unwrap().is_none());
    }

    #[test]
    fn pin_archive_tag_round_trip_and_listing() {
        let repo = repo();
        repo.save_record(&ws(), &record()).unwrap();

        assert!(repo.set_pinned(&ws(), "sum_1", true).unwrap());
        assert!(repo
            .set_tags(&ws(), "sum_1", &["a".into(), "b".into()])
            .unwrap());
        let got = repo.get_record(&ws(), "sum_1").unwrap().unwrap();
        assert!(got.pinned);
        assert_eq!(got.tags, vec!["a".to_string(), "b".to_string()]);

        // Listed while active.
        assert_eq!(repo.list_records(&ws(), false, 10).unwrap().len(), 1);
        // Archiving hides it from the default list but not the include-archived one.
        assert!(repo.set_archived(&ws(), "sum_1", true).unwrap());
        assert!(repo.list_records(&ws(), false, 10).unwrap().is_empty());
        assert_eq!(repo.list_records(&ws(), true, 10).unwrap().len(), 1);
    }

    #[test]
    fn list_orders_pinned_first_then_newest() {
        let repo = repo();
        for (id, created, pinned) in [("old", 100, false), ("new", 300, false), ("pin", 200, true)]
        {
            let mut r = record();
            r.id = id.into();
            r.created_at = created;
            r.pinned = pinned;
            repo.save_record(&ws(), &r).unwrap();
        }
        let ids: Vec<String> = repo
            .list_records(&ws(), false, 10)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        // Pinned first, then newest-by-created.
        assert_eq!(ids, vec!["pin", "new", "old"]);
    }

    #[test]
    fn set_on_missing_record_returns_false() {
        let repo = repo();
        assert!(!repo.set_pinned(&ws(), "ghost", true).unwrap());
    }

    /// Seed three distinct records for search tests.
    fn seed_three(repo: &SqliteRepository) {
        let mut a = record();
        a.id = "a".into();
        a.created_at = 100;
        a.summary.text = "Quarterly planning recap".into();
        a.summary.key_points = vec!["roadmap agreed".into()];
        a.summary.participants = vec!["Alice".into(), "Bob".into()];
        a.tags = vec!["planning".into()];

        let mut b = record();
        b.id = "b".into();
        b.created_at = 200;
        b.summary.text = "Release shipped".into();
        b.summary.key_points = vec!["changelog written".into()];
        b.summary.participants = vec!["Carol".into()];
        b.tags = vec!["release".into()];

        let mut c = record();
        c.id = "c".into();
        c.created_at = 300;
        c.summary.text = "Incident postmortem".into();
        c.summary.key_points = vec!["root cause: roadmap drift".into()];
        c.summary.participants = vec!["Bob".into()];
        c.tags = vec!["ops".into()];

        for r in [a, b, c] {
            repo.save_record(&ws(), &r).unwrap();
        }
    }

    #[test]
    fn delete_record_removes_row_and_children_and_is_scoped() {
        let repo = repo();
        repo.save_record(&ws(), &record()).unwrap(); // record() has action items + a citation
                                                     // Wrong workspace can't delete it.
        let other = WorkspaceId::parse("ws-other").unwrap();
        assert!(!repo.delete_record(&other, "sum_1").unwrap());
        assert!(repo.get_record(&ws(), "sum_1").unwrap().is_some());

        // Owner deletes it; the row and its children are gone.
        assert!(repo.delete_record(&ws(), "sum_1").unwrap());
        assert!(repo.get_record(&ws(), "sum_1").unwrap().is_none());
        // Re-fetch confirms child rows didn't resurrect a partial record.
        let orphan_actions: i64 = repo
            .conn
            .query_row(
                "SELECT COUNT(*) FROM summary_action_items WHERE summary_id = 'sum_1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphan_actions, 0);
        // Deleting again is a no-op.
        assert!(!repo.delete_record(&ws(), "sum_1").unwrap());
    }

    #[test]
    fn search_by_text_matches_text_or_key_points_case_insensitively() {
        let repo = repo();
        seed_three(&repo);
        // "roadmap" appears in a.key_points and c.key_points; newest (c) first.
        let q = SummaryQuery {
            text: Some("ROADMAP".into()),
            ..Default::default()
        };
        let ids: Vec<String> = repo
            .search_records(&ws(), &q)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, vec!["c", "a"]);
    }

    #[test]
    fn search_by_participant_and_by_tag() {
        let repo = repo();
        seed_three(&repo);
        let by_part = repo
            .search_records(
                &ws(),
                &SummaryQuery {
                    participant: Some("bob".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let ids: Vec<&str> = by_part.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["c", "a"]); // Bob is in a and c

        let by_tag = repo
            .search_records(
                &ws(),
                &SummaryQuery {
                    tag: Some("release".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(by_tag.len(), 1);
        assert_eq!(by_tag[0].id, "b");
    }

    #[test]
    fn search_paginates_with_limit_and_offset() {
        let repo = repo();
        seed_three(&repo); // newest-first: c, b, a
        let page1 = repo
            .search_records(
                &ws(),
                &SummaryQuery {
                    limit: 2,
                    offset: 0,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            page1.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["c", "b"]
        );
        let page2 = repo
            .search_records(
                &ws(),
                &SummaryQuery {
                    limit: 2,
                    offset: 2,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            page2.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["a"]
        );
    }

    #[test]
    fn search_combines_filters_and_is_workspace_scoped() {
        let repo = repo();
        seed_three(&repo);
        // participant Bob AND text "postmortem" → only c.
        let q = SummaryQuery {
            participant: Some("Bob".into()),
            text: Some("postmortem".into()),
            ..Default::default()
        };
        let res = repo.search_records(&ws(), &q).unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].id, "c");

        // Another workspace sees nothing.
        let other = WorkspaceId::parse("ws-other").unwrap();
        assert!(repo
            .search_records(&other, &SummaryQuery::default())
            .unwrap()
            .is_empty());
    }
}
