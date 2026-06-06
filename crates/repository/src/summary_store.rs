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

/// A stored summary with its provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRecord {
    pub id: String,
    /// The channel/scope this summary covered, if any.
    pub channel_id: Option<ChannelId>,
    pub model: String,
    pub cost_micros: i64,
    pub degraded: bool,
    pub created_at: i64,
    pub summary: ExtractedSummary,
}

/// Storage boundary for structured summaries.
pub trait StructuredSummaryRepository {
    fn save_record(&self, workspace: &WorkspaceId, record: &SummaryRecord) -> Result<()>;
    fn get_record(&self, workspace: &WorkspaceId, id: &str) -> Result<Option<SummaryRecord>>;
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
                text, key_points, technical_terms, participants)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
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
                        text, key_points, technical_terms, participants
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
                    ))
                },
            )
            .optional()?;
        let Some((channel, model, cost, degraded, created, text, kp, tt, parts)) = main else {
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
}
