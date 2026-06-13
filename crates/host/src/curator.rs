//! AI wiki curator (PRD §8.2 CUR-*; ADR-127) — host orchestration.
//!
//! The synthesizer ([`crate::wiki`]) organizes a workspace's knowledge units into
//! a topic page. The **curator** keeps that knowledge base healthy: it reviews
//! the stored units and reports **duplicate clusters** (near-identical facts that
//! slipped past the ingest dedup gate — e.g. stored before the gate existed, or
//! just under its threshold) and **stale** units (older than a cutoff), so an
//! admin can prune or re-synthesize.
//!
//! v1 is **advisory**: it never mutates the store, so it's inherently reversible
//! (ADR-127's reversibility requirement). The report is deterministic — clustering
//! is pure cosine over the already-stored embeddings, no LLM call — so it's fully
//! testable offline. Applying suggestions (merge/delete with undo) is a follow-up.

use repository::{KnowledgeRepository, StoredKnowledgeUnit};

/// Default cosine at/above which two same-kind units are deemed duplicates. Set
/// higher than the ingest near-dup gate (0.93) so the curator flags only clearly
/// redundant facts for review.
pub const DEFAULT_DUP_THRESHOLD: f32 = 0.95;

/// A set of near-identical units. The `canonical` (oldest) is the one to keep;
/// the rest are redundant and safe to prune after review.
#[derive(Debug, Clone, PartialEq)]
pub struct DuplicateCluster {
    pub canonical_id: String,
    pub kind: String,
    pub text: String,
    /// Ids of the redundant units (newest-first), excluding the canonical.
    pub duplicate_ids: Vec<String>,
}

/// A unit old enough to review for staleness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleUnit {
    pub id: String,
    pub kind: String,
    pub text: String,
    pub age_secs: i64,
}

/// The curator's read-only health report for a workspace's knowledge base.
#[derive(Debug, Clone, PartialEq)]
pub struct CurationReport {
    pub total_units: usize,
    /// Units that carry an embedding (only these can be duplicate-clustered).
    pub embedded_units: usize,
    pub duplicate_clusters: Vec<DuplicateCluster>,
    pub stale: Vec<StaleUnit>,
}

impl CurationReport {
    /// Total redundant units across all duplicate clusters (prune candidates).
    pub fn redundant_count(&self) -> usize {
        self.duplicate_clusters.iter().map(|c| c.duplicate_ids.len()).sum()
    }
}

/// Reviews a workspace's knowledge units; never mutates (advisory).
pub struct CuratorService<'a, R> {
    repo: &'a R,
    dup_threshold: f32,
}

impl<'a, R: KnowledgeRepository> CuratorService<'a, R> {
    pub fn new(repo: &'a R) -> Self {
        Self {
            repo,
            dup_threshold: DEFAULT_DUP_THRESHOLD,
        }
    }

    /// Override the duplicate cosine threshold.
    pub fn with_dup_threshold(mut self, threshold: f32) -> Self {
        self.dup_threshold = threshold;
        self
    }

    /// Build the curation report: duplicate clusters + units older than
    /// `stale_after_secs` (relative to `now`).
    pub fn curate(
        &self,
        workspace: &domain::WorkspaceId,
        now: i64,
        stale_after_secs: i64,
    ) -> anyhow::Result<CurationReport> {
        // Oldest-first so a cluster's canonical (the unit to keep) is the original.
        let mut units = self.repo.list_units(workspace)?;
        units.sort_by_key(|u| (u.created_at, u.id.clone()));
        let total_units = units.len();
        let embedded_units = units.iter().filter(|u| u.embedding.is_some()).count();

        let duplicate_clusters = self.cluster_duplicates(&units);
        let stale = units
            .iter()
            .filter(|u| now.saturating_sub(u.created_at) >= stale_after_secs)
            .map(|u| StaleUnit {
                id: u.id.clone(),
                kind: u.kind.clone(),
                text: u.text.clone(),
                age_secs: now.saturating_sub(u.created_at),
            })
            .collect();

        Ok(CurationReport {
            total_units,
            embedded_units,
            duplicate_clusters,
            stale,
        })
    }

    /// Greedy single-link clustering over same-kind embedded units: the first
    /// (oldest) unembedded-into-a-cluster unit seeds a cluster and absorbs every
    /// later same-kind unit within `dup_threshold`. Only clusters of ≥2 are kept.
    fn cluster_duplicates(&self, units_oldest_first: &[StoredKnowledgeUnit]) -> Vec<DuplicateCluster> {
        let mut clustered = vec![false; units_oldest_first.len()];
        let mut out = Vec::new();
        for i in 0..units_oldest_first.len() {
            if clustered[i] {
                continue;
            }
            let Some(ei) = &units_oldest_first[i].embedding else {
                continue;
            };
            let mut dup_ids = Vec::new();
            // Newer units (higher index) that are near-identical → duplicates.
            for (j, item) in units_oldest_first.iter().enumerate().skip(i + 1) {
                if clustered[j] || item.kind != units_oldest_first[i].kind {
                    continue;
                }
                if let Some(ej) = &item.embedding {
                    if ej.len() == ei.len()
                        && domain::cosine_similarity(ei, ej) >= self.dup_threshold
                    {
                        clustered[j] = true;
                        dup_ids.push(item.id.clone());
                    }
                }
            }
            if !dup_ids.is_empty() {
                clustered[i] = true;
                dup_ids.reverse(); // newest-first
                out.push(DuplicateCluster {
                    canonical_id: units_oldest_first[i].id.clone(),
                    kind: units_oldest_first[i].kind.clone(),
                    text: units_oldest_first[i].text.clone(),
                    duplicate_ids: dup_ids,
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use repository::{KnowledgeRepository, SqliteRepository};

    fn ws() -> domain::WorkspaceId {
        domain::WorkspaceId::parse("ws-1").unwrap()
    }

    fn unit(id: &str, kind: &str, text: &str, emb: Vec<f32>, created: i64) -> StoredKnowledgeUnit {
        StoredKnowledgeUnit {
            id: id.into(),
            summary_id: "s".into(),
            kind: kind.into(),
            text: text.into(),
            source_ids: vec![],
            embedding: Some(emb),
            model: Some("fixed".into()),
            created_at: created,
        }
    }

    #[test]
    fn reports_duplicate_clusters_and_stale_units() {
        let repo = SqliteRepository::in_memory().unwrap();
        // Two near-identical key points (same vector) created at 100 and 300, plus
        // a distinct one. The oldest near-dup is the canonical.
        repo.save_units(
            &ws(),
            &[
                unit("a", "key_point", "migration thursday", vec![0.0, 0.0, 1.0, 0.0], 100),
                unit("b", "key_point", "migration is thursday", vec![0.0, 0.0, 1.0, 0.0], 300),
                unit("c", "key_point", "ship friday", vec![0.0, 0.0, 0.0, 1.0], 400),
            ],
        )
        .unwrap();

        // now=1000, stale cutoff 500s → units created at/before 500 are stale.
        let report = CuratorService::new(&repo).curate(&ws(), 1_000, 500).unwrap();
        assert_eq!(report.total_units, 3);
        assert_eq!(report.embedded_units, 3);
        assert_eq!(report.duplicate_clusters.len(), 1);
        let cluster = &report.duplicate_clusters[0];
        assert_eq!(cluster.canonical_id, "a"); // oldest is canonical
        assert_eq!(cluster.duplicate_ids, vec!["b".to_string()]);
        assert_eq!(report.redundant_count(), 1);
        // a (100) and b (300) are ≥500s old; c (400) is 600s old → all three stale.
        assert_eq!(report.stale.len(), 3);
    }

    #[test]
    fn no_duplicates_or_stale_when_distinct_and_recent() {
        let repo = SqliteRepository::in_memory().unwrap();
        repo.save_units(
            &ws(),
            &[
                unit("a", "key_point", "alpha", vec![1.0, 0.0, 0.0, 0.0], 900),
                unit("b", "key_point", "beta", vec![0.0, 1.0, 0.0, 0.0], 950),
            ],
        )
        .unwrap();
        let report = CuratorService::new(&repo).curate(&ws(), 1_000, 500).unwrap();
        assert!(report.duplicate_clusters.is_empty());
        assert!(report.stale.is_empty());
    }

    #[test]
    fn different_kinds_are_not_clustered_together() {
        let repo = SqliteRepository::in_memory().unwrap();
        // Same vector but different kinds → not duplicates of each other.
        repo.save_units(
            &ws(),
            &[
                unit("a", "headline", "x", vec![1.0, 0.0, 0.0, 0.0], 1),
                unit("b", "key_point", "x", vec![1.0, 0.0, 0.0, 0.0], 2),
            ],
        )
        .unwrap();
        let report = CuratorService::new(&repo).curate(&ws(), 10, 100_000).unwrap();
        assert!(report.duplicate_clusters.is_empty());
    }
}
