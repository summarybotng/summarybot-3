//! Knowledge-unit persistence (PRD §8.1; ADR-127).
//!
//! Stores units extracted from summaries with their embedding (f32 little-endian
//! blob) for semantic search. v1 search is brute-force cosine in the host over
//! `list_units` (the PRD-sanctioned Rust-native fallback, KNO-006); a `model` tag
//! pins which embedder produced each vector. Workspace-scoped (TEN-007).

use crate::SqliteRepository;
use anyhow::Result;
use domain::WorkspaceId;
use rusqlite::params;

/// A knowledge unit as persisted.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredKnowledgeUnit {
    pub id: String,
    pub summary_id: String,
    /// `UnitKind` as a string ("headline" / "key_point" / "action_item").
    pub kind: String,
    pub text: String,
    /// Provenance: source message ids (COH-005).
    pub source_ids: Vec<String>,
    /// Embedding vector; `None` if ingestion couldn't embed it yet.
    pub embedding: Option<Vec<f32>>,
    /// Embedding model id (pinned; Q#8) — `None` when unembedded.
    pub model: Option<String>,
    pub created_at: i64,
}

/// Storage boundary for knowledge units.
pub trait KnowledgeRepository {
    /// Insert/replace units (keyed by id).
    fn save_units(&self, workspace: &WorkspaceId, units: &[StoredKnowledgeUnit]) -> Result<()>;
    /// All units for a workspace (with embeddings) — the brute-force search set.
    fn list_units(&self, workspace: &WorkspaceId) -> Result<Vec<StoredKnowledgeUnit>>;
    /// Count of units in a workspace.
    fn count_units(&self, workspace: &WorkspaceId) -> Result<i64>;
}

/// Encode an f32 vector as little-endian bytes for the `embedding` blob.
fn encode_embedding(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Decode a little-endian f32 blob back to a vector (drops a trailing partial).
fn decode_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

impl KnowledgeRepository for SqliteRepository {
    fn save_units(&self, workspace: &WorkspaceId, units: &[StoredKnowledgeUnit]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for u in units {
            tx.execute(
                "INSERT INTO knowledge_units
                     (id, workspace_id, summary_id, kind, text, source_ids, embedding, model, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET
                     text = excluded.text,
                     source_ids = excluded.source_ids,
                     embedding = excluded.embedding,
                     model = excluded.model",
                params![
                    u.id,
                    workspace.as_str(),
                    u.summary_id,
                    u.kind,
                    u.text,
                    u.source_ids.join("\n"),
                    u.embedding.as_deref().map(encode_embedding),
                    u.model,
                    u.created_at,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn list_units(&self, workspace: &WorkspaceId) -> Result<Vec<StoredKnowledgeUnit>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, summary_id, kind, text, source_ids, embedding, model, created_at
             FROM knowledge_units WHERE workspace_id = ?1
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![workspace.as_str()], |row| {
            let source_ids: String = row.get(4)?;
            let embedding: Option<Vec<u8>> = row.get(5)?;
            Ok(StoredKnowledgeUnit {
                id: row.get(0)?,
                summary_id: row.get(1)?,
                kind: row.get(2)?,
                text: row.get(3)?,
                source_ids: source_ids
                    .split('\n')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect(),
                embedding: embedding.map(|b| decode_embedding(&b)),
                model: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn count_units(&self, workspace: &WorkspaceId) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM knowledge_units WHERE workspace_id = ?1",
            params![workspace.as_str()],
            |row| row.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> SqliteRepository {
        SqliteRepository::in_memory().unwrap()
    }
    fn ws(id: &str) -> WorkspaceId {
        WorkspaceId::parse(id).unwrap()
    }

    #[test]
    fn save_list_roundtrips_with_embedding_and_provenance() {
        let repo = repo();
        let unit = StoredKnowledgeUnit {
            id: "ku_1".into(),
            summary_id: "sum_1".into(),
            kind: "key_point".into(),
            text: "Launch on Friday".into(),
            source_ids: vec!["m0".into(), "m1".into()],
            embedding: Some(vec![0.1, -0.2, 0.3]),
            model: Some("nomic-embed-text".into()),
            created_at: 100,
        };
        repo.save_units(&ws("w1"), std::slice::from_ref(&unit))
            .unwrap();

        let got = repo.list_units(&ws("w1")).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], unit);
        assert_eq!(repo.count_units(&ws("w1")).unwrap(), 1);
        // Scoped per workspace.
        assert!(repo.list_units(&ws("w2")).unwrap().is_empty());

        // Upsert replaces in place (re-embed).
        let mut updated = unit.clone();
        updated.embedding = Some(vec![1.0, 0.0, 0.0]);
        repo.save_units(&ws("w1"), std::slice::from_ref(&updated))
            .unwrap();
        assert_eq!(repo.count_units(&ws("w1")).unwrap(), 1);
        assert_eq!(
            repo.list_units(&ws("w1")).unwrap()[0].embedding,
            updated.embedding
        );
    }
}
