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
use rusqlite::{params, OptionalExtension};

/// A stored summary with its provenance and management flags (§5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Pin/unpin (§5.1). Returns whether a row changed.
    fn set_pinned(&self, workspace: &WorkspaceId, id: &str, pinned: bool) -> Result<bool>;
    /// Archive/unarchive (§5.1).
    fn set_archived(&self, workspace: &WorkspaceId, id: &str, archived: bool) -> Result<bool>;
    /// Replace the tag set (§5.1).
    fn set_tags(&self, workspace: &WorkspaceId, id: &str, tags: &[String]) -> Result<bool>;
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
                text, key_points, technical_terms, participants, pinned, archived, tags)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
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
                        text, key_points, technical_terms, participants, pinned, archived, tags
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
}
