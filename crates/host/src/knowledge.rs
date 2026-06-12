//! Knowledge ingestion + semantic search (PRD §8.1; ADR-127) — host orchestration.
//!
//! Extracts knowledge units from a produced summary (pure: [`domain::extract_units`]),
//! embeds them via an [`Embedder`] (a local model over HTTP, or a deterministic
//! demo embedder for the offline/default build), and stores them. Search embeds
//! the query and ranks stored units by cosine ([`domain::rank_by_cosine`]).
//! Brute-force over the workspace's units — the ADR-127 v1 store.

use domain::summarize::ExtractedSummary;
use repository::{KnowledgeRepository, StoredKnowledgeUnit};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// Content-addressed knowledge-unit id (ADR-129 Layer 1): `ku_<sha256(workspace
/// : kind : normalized_text)>`. Independent of which summary/run produced the
/// fact, so the same fact re-extracted by rolling re-summarization or a re-run
/// maps to the **same id** and collapses on upsert instead of duplicating in the
/// vector store. The hash key format is load-bearing — changing it invalidates
/// existing ids. (Per-source scoping is a documented refinement; v1 dedups within
/// the workspace.)
pub fn knowledge_unit_id(workspace: &str, kind: &str, text: &str) -> String {
    // Stable normalization: lowercase, whitespace-collapsed, trimmed.
    let normalized = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let mut hasher = Sha256::new();
    hasher.update(workspace.as_bytes());
    hasher.update([0x1f]);
    hasher.update(kind.as_bytes());
    hasher.update([0x1f]);
    hasher.update(normalized.as_bytes());
    format!("ku_{:x}", hasher.finalize())
}

/// Produces embedding vectors for texts. Batched; the model id is pinned with
/// each stored vector (Q#8). `Send + Sync` so it can live in shared app state.
pub trait Embedder: Send + Sync {
    fn model(&self) -> &str;
    /// Embed each input; returns one vector per input (same order).
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String>;
}

/// Deterministic, network-free embedder for the default build + tests: hashes
/// each token into a fixed-dim bag-of-words vector, then L2-normalizes. Not
/// semantically strong, but stable and good enough to exercise the pipeline and
/// rank exact/near term overlaps.
pub struct DemoEmbedder {
    dim: usize,
}

impl Default for DemoEmbedder {
    fn default() -> Self {
        Self { dim: 64 }
    }
}

impl Embedder for DemoEmbedder {
    fn model(&self) -> &str {
        "demo-hash-64"
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        Ok(texts.iter().map(|t| hash_embed(t, self.dim)).collect())
    }
}

fn hash_embed(text: &str, dim: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; dim];
    for tok in text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
    {
        // FNV-1a over the token → bucket.
        let mut h: u64 = 0xcbf29ce484222325;
        for b in tok.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        v[(h as usize) % dim] += 1.0;
    }
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

/// A search result: the matched unit plus its similarity score.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub id: String,
    pub summary_id: String,
    pub kind: String,
    pub text: String,
    pub source_ids: Vec<String>,
    pub score: f32,
}

/// Orchestrates knowledge ingestion + search over a repository + embedder.
pub struct KnowledgeService<'a, R> {
    repo: &'a R,
    embedder: &'a dyn Embedder,
}

impl<'a, R: KnowledgeRepository> KnowledgeService<'a, R> {
    pub fn new(repo: &'a R, embedder: &'a dyn Embedder) -> Self {
        Self { repo, embedder }
    }

    /// Extract + embed + store the units of a produced summary (KNO-001..003).
    /// Embedding is best-effort: if the embedder fails, units are stored
    /// unembedded (recorded, but not searchable until re-embedded) and the call
    /// still succeeds — ingestion never fails the summary. Returns units stored.
    pub fn ingest(
        &self,
        workspace: &domain::WorkspaceId,
        summary: &ExtractedSummary,
        summary_id: &str,
        now: i64,
    ) -> anyhow::Result<usize> {
        let units = domain::extract_units(summary, summary_id);
        if units.is_empty() {
            return Ok(0);
        }
        let texts: Vec<String> = units.iter().map(|u| u.text.clone()).collect();
        let embeddings = self.embedder.embed(&texts).ok();
        let model = self.embedder.model();
        // Content-addressed ids (ADR-129): dedup exact repeats within this batch
        // and across summaries/runs (the repo upserts on id).
        let mut seen = std::collections::HashSet::new();
        let stored: Vec<StoredKnowledgeUnit> = units
            .iter()
            .enumerate()
            .map(|(i, u)| StoredKnowledgeUnit {
                id: knowledge_unit_id(workspace.as_str(), u.kind.as_str(), &u.text),
                summary_id: u.summary_id.clone(),
                kind: u.kind.as_str().to_string(),
                text: u.text.clone(),
                source_ids: u
                    .source_message_ids
                    .iter()
                    .map(|m| m.as_str().to_string())
                    .collect(),
                embedding: embeddings.as_ref().and_then(|e| e.get(i).cloned()),
                model: embeddings.as_ref().map(|_| model.to_string()),
                created_at: now,
            })
            .filter(|u| seen.insert(u.id.clone()))
            .collect();
        self.repo.save_units(workspace, &stored)?;
        Ok(stored.len())
    }

    /// Semantic search: embed `query`, rank the workspace's embedded units by
    /// cosine, return the top `k` (KNO-005).
    pub fn search(
        &self,
        workspace: &domain::WorkspaceId,
        query: &str,
        k: usize,
    ) -> anyhow::Result<Vec<SearchHit>> {
        let query_vec = self
            .embedder
            .embed(&[query.to_string()])
            .map_err(|e| anyhow::anyhow!("embed query: {e}"))?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("embedder returned no vector"))?;

        let units = self.repo.list_units(workspace)?;
        let candidates: Vec<(String, Vec<f32>)> = units
            .iter()
            .filter_map(|u| u.embedding.clone().map(|e| (u.id.clone(), e)))
            .collect();
        let ranked = domain::rank_by_cosine(&query_vec, &candidates, k);

        let by_id: HashMap<&str, &StoredKnowledgeUnit> =
            units.iter().map(|u| (u.id.as_str(), u)).collect();
        Ok(ranked
            .into_iter()
            .filter_map(|(id, score)| {
                by_id.get(id.as_str()).map(|u| SearchHit {
                    id: u.id.clone(),
                    summary_id: u.summary_id.clone(),
                    kind: u.kind.clone(),
                    text: u.text.clone(),
                    source_ids: u.source_ids.clone(),
                    score,
                })
            })
            .collect())
    }
}

/// OpenAI-compatible HTTP embedder (`POST {base_url}/embeddings`) — the local
/// self-hosted model (Mac-mini Ollama `nomic-embed-text`). Feature-gated so the
/// default build stays offline.
#[cfg(feature = "http-llm")]
pub struct HttpEmbedder {
    base_url: String,
    api_key: Option<domain::Secret<String>>,
    model: String,
}

#[cfg(feature = "http-llm")]
impl HttpEmbedder {
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<domain::Secret<String>>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            model: model.into(),
        }
    }
}

#[cfg(feature = "http-llm")]
impl Embedder for HttpEmbedder {
    fn model(&self) -> &str {
        &self.model
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let endpoint = format!("{}/embeddings", self.base_url.trim_end_matches('/'));
        let body = serde_json::json!({ "model": self.model, "input": texts });
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(60))
            .build();
        let mut req = agent
            .post(&endpoint)
            .set("Content-Type", "application/json");
        if let Some(key) = &self.api_key {
            req = req.set("Authorization", &format!("Bearer {}", key.expose_secret()));
        }
        let resp = match req.send_json(body) {
            Ok(r) => r,
            Err(ureq::Error::Status(code, _)) => return Err(format!("embeddings http {code}")),
            Err(ureq::Error::Transport(t)) => return Err(format!("embeddings transport: {t}")),
        };
        let v: serde_json::Value = resp
            .into_json()
            .map_err(|e| format!("embeddings json: {e}"))?;
        let data = v
            .get("data")
            .and_then(|d| d.as_array())
            .ok_or("embeddings response missing data[]")?;
        let mut out = Vec::with_capacity(data.len());
        for item in data {
            let vec = item
                .get("embedding")
                .and_then(|e| e.as_array())
                .ok_or("embeddings item missing embedding[]")?
                .iter()
                .map(|x| x.as_f64().unwrap_or(0.0) as f32)
                .collect();
            out.push(vec);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::summarize::ExtractedSummary;
    use repository::SqliteRepository;

    fn ws() -> domain::WorkspaceId {
        domain::WorkspaceId::parse("ws-1").unwrap()
    }

    #[test]
    fn demo_embedder_is_deterministic_and_normalized() {
        let e = DemoEmbedder::default();
        let a = &e.embed(&["launch on friday".into()]).unwrap()[0];
        let b = &e.embed(&["launch on friday".into()]).unwrap()[0];
        assert_eq!(a, b);
        let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    fn summary(text: &str, points: &[&str]) -> ExtractedSummary {
        ExtractedSummary {
            text: text.into(),
            key_points: points.iter().map(|p| p.to_string()).collect(),
            action_items: vec![],
            technical_terms: vec![],
            participants: vec![],
            citations: vec![],
        }
    }

    #[test]
    fn ingest_then_search_ranks_the_relevant_unit_first() {
        let repo = SqliteRepository::in_memory().unwrap();
        let embedder = DemoEmbedder::default();
        let svc = KnowledgeService::new(&repo, &embedder);

        let n = svc
            .ingest(
                &ws(),
                &summary(
                    "Pricing and launch plans",
                    &[
                        "Launch the pricing page on Friday",
                        "Database migration must run first",
                    ],
                ),
                "sum_1",
                100,
            )
            .unwrap();
        assert_eq!(n, 3); // headline + 2 key points

        // A query about the database ranks the migration unit on top.
        let hits = svc.search(&ws(), "database migration", 3).unwrap();
        assert!(!hits.is_empty());
        assert!(hits[0].text.to_lowercase().contains("migration"));
        assert!(hits[0].score > 0.0);
    }

    #[test]
    fn content_hash_ids_dedup_repeats_and_shared_facts() {
        use repository::KnowledgeRepository;
        let repo = SqliteRepository::in_memory().unwrap();
        let embedder = DemoEmbedder::default();
        let svc = KnowledgeService::new(&repo, &embedder);

        let s = summary(
            "Launch plan",
            &["Ship the pricing page Friday", "Run the migration first"],
        );
        assert_eq!(svc.ingest(&ws(), &s, "sum_1", 100).unwrap(), 3); // headline + 2

        // Re-ingesting the SAME content under a different summary id stores nothing
        // new — content-addressed ids collapse the repeats (ADR-129 Layer 1).
        svc.ingest(&ws(), &s, "sum_2", 200).unwrap();
        assert_eq!(repo.count_units(&ws()).unwrap(), 3);

        // A new summary sharing one key point adds only the genuinely-new units:
        // its headline + the one new key point (the shared one collapses).
        let s2 = summary(
            "Different topic",
            &["Run the migration first", "Brand new decision was made"],
        );
        svc.ingest(&ws(), &s2, "sum_3", 300).unwrap();
        assert_eq!(repo.count_units(&ws()).unwrap(), 5);
    }

    #[test]
    fn search_is_empty_when_nothing_ingested() {
        let repo = SqliteRepository::in_memory().unwrap();
        let embedder = DemoEmbedder::default();
        let svc = KnowledgeService::new(&repo, &embedder);
        assert!(svc.search(&ws(), "anything", 5).unwrap().is_empty());
    }
}
