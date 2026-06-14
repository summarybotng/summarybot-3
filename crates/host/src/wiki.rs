//! Wiki synthesis (PRD §8.2 WIK-001..003; ADR-127) — host orchestration.
//!
//! A workspace's knowledge units (the headlines / key points / action items
//! extracted from its summaries) are emergent, scattered facts. Synthesis asks
//! the LLM to **organize** them into one coherent markdown page grouped by topic
//! (WIK-001), stored as a regenerable [`WikiPage`] keyed `knowledge-base`
//! (WIK-003). It's a single LLM pass over the unit texts — no map-reduce, since
//! v1 caps units per summary and we synthesize the most recent window.
//!
//! Pure prompt assembly ([`build_synthesis_prompt`]) is unit-tested; the LLM
//! call rides the shared [`ResilientLlm`] (rate-limit + retry) like summaries.

use crate::llm::{LlmClient, LlmProvider, LlmRequest, RequestPriority, ResilientLlm};
use domain::summarize::ModelLadder;
use domain::{FailureClass, WorkspaceId};
use repository::{KnowledgeRepository, StoredKnowledgeUnit, WikiPage, WikiRepository};

/// The single emergent page v1 maintains per workspace (WIK-003).
pub const KNOWLEDGE_BASE_SLUG: &str = "knowledge-base";
/// Cap on units fed into one synthesis prompt — the most recent N, so a large
/// history stays within a single context window (the rest already shaped the
/// earlier units they were synthesized alongside).
const MAX_UNITS_PER_SYNTHESIS: usize = 200;
/// Output-token budget for the synthesized page.
const SYNTHESIS_OUTPUT_TOKENS: i64 = 1500;
/// Tokens reserved for the instruction preamble when costing.
const PROMPT_OVERHEAD_TOKENS: i64 = 256;

/// A synthesized page plus its cost + provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct WikiOutcome {
    pub page: WikiPage,
    pub model: String,
    pub cost_micros: i64,
}

/// Why synthesis failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WikiError {
    /// No knowledge units yet — nothing to synthesize (WIK-002).
    NoUnits,
    /// The LLM chain gave up; carries the classified reason.
    Llm(FailureClass),
    /// The cap can't afford even the cheapest model.
    CostCapTooLow,
}

impl std::fmt::Display for WikiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WikiError::NoUnits => write!(f, "no knowledge units to synthesize"),
            WikiError::Llm(c) => write!(f, "llm failure: {}", c.as_str()),
            WikiError::CostCapTooLow => write!(f, "cost cap too low for synthesis"),
        }
    }
}

/// Estimate tokens the same coarse way the summarizer does (≈4 chars/token).
fn estimate_tokens(text: &str) -> i64 {
    (text.len() as i64).div_euclid(4).max(1)
}

/// Build the synthesis prompt: group the units' facts under topic headings,
/// markdown out. Pure + deterministic for the given units, so it's testable.
pub fn build_synthesis_prompt(units: &[StoredKnowledgeUnit]) -> String {
    let mut facts = String::new();
    for u in units {
        facts.push_str("- (");
        facts.push_str(&u.kind);
        facts.push_str(") ");
        facts.push_str(u.text.trim());
        facts.push('\n');
    }
    format!(
        "You are maintaining a team knowledge base. Below are facts extracted from \
recent conversation summaries. Organize them into a single, coherent Markdown \
document grouped under topic headings (`## Topic`). Merge duplicates, group \
related facts, and keep each fact concise. Do not invent facts that are not \
present below. Output only the Markdown document, starting with a top-level \
`# Knowledge Base` heading.\n\nFacts:\n{facts}"
    )
}

/// Orchestrates wiki synthesis over a repository (knowledge + wiki) + LLM engine.
pub struct WikiService<'a, R, C: LlmClient> {
    repo: &'a R,
    engine: &'a ResilientLlm<C>,
    ladder: &'a ModelLadder,
}

impl<'a, R, C> WikiService<'a, R, C>
where
    R: KnowledgeRepository + WikiRepository,
    C: LlmClient,
{
    pub fn new(repo: &'a R, engine: &'a ResilientLlm<C>, ladder: &'a ModelLadder) -> Self {
        Self {
            repo,
            engine,
            ladder,
        }
    }

    /// Regenerate the workspace's `knowledge-base` page from its units
    /// (WIK-001..003). One LLM pass over the most recent units; the result is
    /// upserted (replacing the prior page) and returned with its cost.
    pub fn synthesize(
        &self,
        workspace: &WorkspaceId,
        provider: LlmProvider,
        priority: RequestPriority,
        cap_micros: i64,
        now: i64,
    ) -> Result<WikiOutcome, WikiError> {
        let mut units = self
            .repo
            .list_units(workspace)
            .map_err(|_| WikiError::Llm(FailureClass::Unknown))?;
        if units.is_empty() {
            return Err(WikiError::NoUnits);
        }
        // Synthesize over the most recent window (newest last in storage order;
        // keep the tail).
        let unit_count = units.len() as i64;
        if units.len() > MAX_UNITS_PER_SYNTHESIS {
            units = units.split_off(units.len() - MAX_UNITS_PER_SYNTHESIS);
        }

        let prompt = build_synthesis_prompt(&units);
        let input_tokens = estimate_tokens(&prompt) + PROMPT_OVERHEAD_TOKENS;

        // Cost-gate against the start model; downgrade to a cheaper rung if the
        // cap won't fit, mirroring the summarizer's Q#5 policy.
        let mut current = self
            .ladder
            .start_index(domain::summarize::SummaryLength::Detailed);
        let response = loop {
            let model = self.ladder.get(current).expect("ladder index in range");
            let est = model
                .price
                .cost_micros(input_tokens, SYNTHESIS_OUTPUT_TOKENS);
            if est > cap_micros {
                if current > 0 {
                    current -= 1;
                    continue;
                }
                return Err(WikiError::CostCapTooLow);
            }
            let request = LlmRequest {
                provider,
                priority,
                model: model.name.clone(),
                prompt: prompt.clone(),
            };
            match self.engine.complete(&request) {
                Ok(resp) => break resp,
                Err(err) => return Err(WikiError::Llm(err.class)),
            }
        };

        let model = self.ladder.get(current).expect("ladder index in range");
        let out_tokens = estimate_tokens(&response.text);
        let cost_micros = model.price.cost_micros(input_tokens, out_tokens);

        let content_md = response.text.trim().to_string();
        let page = WikiPage {
            slug: KNOWLEDGE_BASE_SLUG.to_string(),
            title: "Knowledge Base".to_string(),
            content_md,
            unit_count,
            updated_at: now,
        };
        self.repo
            .upsert_page(workspace, &page)
            .map_err(|_| WikiError::Llm(FailureClass::Unknown))?;

        Ok(WikiOutcome {
            page,
            model: response.model,
            cost_micros,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{DemoLlmClient, GlobalRateLimiter, RateLimitConfig};
    use domain::summarize::{Model, ModelLadder, ModelPrice};
    use repository::SqliteRepository;
    use std::sync::Arc;

    fn ws() -> WorkspaceId {
        WorkspaceId::parse("ws-1").unwrap()
    }

    fn ladder() -> ModelLadder {
        ModelLadder::new(vec![Model {
            name: "demo".into(),
            price: ModelPrice {
                input_micros_per_ktoken: 100,
                output_micros_per_ktoken: 300,
            },
            context_tokens: 200_000,
        }])
    }

    fn unit(id: &str, kind: &str, text: &str) -> StoredKnowledgeUnit {
        StoredKnowledgeUnit {
            id: id.into(),
            summary_id: "sum_1".into(),
            kind: kind.into(),
            text: text.into(),
            source_ids: vec![],
            embedding: None,
            model: None,
            created_at: 1,
            source_channel: None,
            source_date: 1,
            confidence: 1.0,
        }
    }

    #[test]
    fn synthesis_prompt_lists_every_fact() {
        let units = [
            unit("a", "headline", "Pricing page launches Friday"),
            unit("b", "action_item", "Run the migration first"),
        ];
        let p = build_synthesis_prompt(&units);
        assert!(p.contains("Pricing page launches Friday"));
        assert!(p.contains("Run the migration first"));
        assert!(p.contains("# Knowledge Base"));
    }

    fn engine() -> ResilientLlm<DemoLlmClient> {
        let limiter = Arc::new(GlobalRateLimiter::new(RateLimitConfig::default()));
        ResilientLlm::new(DemoLlmClient, limiter)
    }

    #[test]
    fn no_units_is_an_error() {
        let repo = SqliteRepository::in_memory().unwrap();
        let eng = engine();
        let ladder = ladder();
        let svc = WikiService::new(&repo, &eng, &ladder);
        let err = svc
            .synthesize(
                &ws(),
                LlmProvider::OpenRouter,
                RequestPriority::Manual,
                i64::MAX,
                100,
            )
            .unwrap_err();
        assert_eq!(err, WikiError::NoUnits);
    }

    #[test]
    fn synthesize_stores_a_knowledge_base_page() {
        let repo = SqliteRepository::in_memory().unwrap();
        repo.save_units(
            &ws(),
            &[
                unit("a", "headline", "Pricing page launches Friday"),
                unit("b", "action_item", "Run the migration first"),
            ],
        )
        .unwrap();
        let eng = engine();
        let ladder = ladder();
        let svc = WikiService::new(&repo, &eng, &ladder);
        let outcome = svc
            .synthesize(
                &ws(),
                LlmProvider::OpenRouter,
                RequestPriority::Manual,
                i64::MAX,
                200,
            )
            .unwrap();
        assert_eq!(outcome.page.slug, KNOWLEDGE_BASE_SLUG);
        assert_eq!(outcome.page.unit_count, 2);
        assert_eq!(outcome.page.updated_at, 200);
        assert!(!outcome.page.content_md.is_empty());
        // Persisted + retrievable.
        let stored = repo.get_page(&ws(), KNOWLEDGE_BASE_SLUG).unwrap().unwrap();
        assert_eq!(stored, outcome.page);
    }
}
