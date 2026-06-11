//! Summarization pipeline service (PRD §12.4, Phase 3) — host orchestration.
//!
//! Ties the pure summarize cores to the resilient LLM client and the job
//! lifecycle: assemble a prompt from the substantial messages, pick a start
//! model by length (ADR-024), guard spend against a hard cap (ADR-095/Q#5),
//! call the [`ResilientLlm`] (which itself does rate-limiting + retry +
//! classification — LEG-001/002/003), then parse + structurally validate the
//! structured output and resolve grounded citations (ADR-004/Q#6).
//!
//! Model fallback lives here: escalate to a stronger model on a recoverable
//! failure or a truncated/empty result; **downgrade** to a cheaper model (flagged
//! `degraded`) when the cap won't fit the current one — the Q#5 best-effort
//! policy. Persisting the structured summary itself is Phase 4 (storage); this
//! produces it and drives the job to a terminal state.

use crate::llm::{LlmClient, LlmProvider, LlmRequest, RequestPriority, ResilientLlm};
use domain::summarize::{
    allocate, finalize, ActionItem, CostGuard, ExtractedSummary, FinishReason, ModelLadder,
    NextModel, QualityError, RawCitation, RawExtraction, ResolvedCitation, SpendDecision,
    SummaryLength,
};
use domain::{
    ChannelId, FailureClass, Job, JobId, JobType, MessageId, NormalizedMessage, Platform,
    WorkspaceId,
};
use repository::JobRepository;
use serde::Deserialize;

/// Tokens reserved for the instruction/system prompt when allocating.
const PROMPT_OVERHEAD_TOKENS: i64 = 512;

/// Cap on citations carried from the map step into a reduced summary, so a very
/// large history doesn't produce an unwieldy citation list.
const MAX_CARRIED_CITATIONS: usize = 50;

/// What a summary request asks for.
pub struct SummarizeRequest<'a> {
    pub messages: &'a [NormalizedMessage],
    pub length: SummaryLength,
    pub provider: LlmProvider,
    pub priority: RequestPriority,
    /// Hard cost cap in micro-dollars (use `i64::MAX` for none).
    pub cap_micros: i64,
    /// Optional per-workspace guidance appended to the prompt (SUM-007), e.g.
    /// "summarize from a product-management perspective; emphasize decisions".
    pub instructions: Option<&'a str>,
}

/// A produced summary plus its cost and provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryOutcome {
    pub summary: ExtractedSummary,
    pub model: String,
    pub cost_micros: i64,
    /// True if the cap forced a cheaper model than the length warranted (Q#5).
    pub degraded: bool,
}

/// Why summarization failed outright.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummarizeError {
    /// Nothing worth summarizing after triviality filtering (MSG-008).
    NoSubstantialMessages,
    /// The LLM chain gave up; carries the classified reason.
    Llm(FailureClass),
    /// The output never validated (truncated/empty/bad citations) on any model.
    Quality(QualityError),
    /// The cap can't afford even the cheapest model.
    CostCapTooLow,
}

impl SummarizeError {
    /// Map to a job `failure_reason` class (LEG-002).
    pub fn failure_class(&self) -> FailureClass {
        match self {
            SummarizeError::NoSubstantialMessages => FailureClass::InvalidRequest,
            SummarizeError::Llm(c) => *c,
            SummarizeError::Quality(_) => FailureClass::Unknown,
            SummarizeError::CostCapTooLow => FailureClass::QuotaExceeded,
        }
    }
}

#[derive(Deserialize)]
struct WireActionItem {
    text: String,
    #[serde(default)]
    assignee: Option<String>,
}

#[derive(Deserialize)]
struct WireCitation {
    message_index: usize,
    #[serde(default)]
    quote: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireExtraction {
    #[serde(default)]
    text: String,
    #[serde(default)]
    key_points: Vec<String>,
    #[serde(default)]
    action_items: Vec<WireActionItem>,
    #[serde(default)]
    technical_terms: Vec<String>,
    #[serde(default)]
    participants: Vec<String>,
    #[serde(default)]
    citations: Vec<WireCitation>,
}

/// Orchestrates one summary over the resilient client + model ladder.
pub struct SummarizationService<'a, C: LlmClient> {
    engine: &'a ResilientLlm<C>,
    ladder: &'a ModelLadder,
}

impl<'a, C: LlmClient> SummarizationService<'a, C> {
    pub fn new(engine: &'a ResilientLlm<C>, ladder: &'a ModelLadder) -> Self {
        Self { engine, ladder }
    }

    /// Run the pipeline. `now` is unused by the pure path but kept for parity
    /// with job-driven callers.
    ///
    /// If the whole transcript fits the start model's context it's a single
    /// pass. Otherwise it's **map-reduce** (ADR-095): split the messages into
    /// window-sized chunks, summarize each into a citation-grounded partial
    /// (map), then summarize the partials into one final summary (reduce,
    /// recursively if the partials themselves overflow a window). The map step's
    /// citations — grounded to real messages — are carried into the final
    /// summary, since the reduce step only sees synthetic partials.
    pub fn summarize(&self, req: &SummarizeRequest) -> Result<SummaryOutcome, SummarizeError> {
        let substantial: Vec<&NormalizedMessage> =
            req.messages.iter().filter(|m| m.is_substantial()).collect();
        if substantial.is_empty() {
            return Err(SummarizeError::NoSubstantialMessages);
        }

        let mut guard = CostGuard::new(req.cap_micros);
        let mut degraded = false;

        // Plan chunking against the *start* model's window (the ladder may still
        // escalate within a window; this only decides map-reduce vs single pass).
        let start = self.ladder.start_index(req.length);
        let context_tokens = self
            .ladder
            .get(start)
            .expect("start index in range")
            .context_tokens;
        let desired_output = output_budget(req.length);
        let total_tokens = estimate_tokens(&assemble_prompt(&substantial, req.instructions));
        let plan = allocate(
            total_tokens,
            context_tokens,
            desired_output,
            PROMPT_OVERHEAD_TOKENS,
        );

        if plan.chunks <= 1 {
            return self.summarize_window(
                req,
                &substantial,
                req.length,
                desired_output,
                &mut guard,
                &mut degraded,
            );
        }

        // Map: a brief partial per contiguous chunk; carry forward real
        // citations. A single chunk that fails the model (an empty/flaky reply,
        // a transient LLM error) shouldn't sink the whole history — skip it and
        // flag the result degraded. A hard cost-cap, though, aborts: the budget
        // is exhausted and later chunks can't fare better.
        let mut partials = Vec::new();
        let mut carried: Vec<ResolvedCitation> = Vec::new();
        let mut last_err = None;
        for (lo, hi) in chunk_ranges(&substantial, plan.input_tokens_per_chunk) {
            match self.summarize_window(
                req,
                &substantial[lo..hi],
                SummaryLength::Brief,
                plan.output_tokens_per_chunk,
                &mut guard,
                &mut degraded,
            ) {
                Ok(part) => {
                    carried.extend(part.summary.citations.iter().cloned());
                    partials.push(part.summary);
                }
                Err(SummarizeError::CostCapTooLow) => return Err(SummarizeError::CostCapTooLow),
                Err(e) => {
                    degraded = true;
                    last_err = Some(e);
                }
            }
        }
        // Every chunk failed — surface the reason rather than an empty summary.
        let Some(first) = partials.first().cloned() else {
            return Err(last_err.unwrap_or(SummarizeError::Quality(QualityError::Empty)));
        };

        let model_name = || self.ladder.get(0).expect("ladder non-empty").name.clone();
        // A single surviving partial needs no reduce pass — it *is* the summary.
        let mut outcome = if partials.len() == 1 {
            SummaryOutcome {
                summary: first,
                model: model_name(),
                cost_micros: guard.spent_micros(),
                degraded,
            }
        } else {
            match self.reduce(req, partials, context_tokens, &mut guard, &mut degraded) {
                Ok(o) => o,
                Err(SummarizeError::CostCapTooLow) => return Err(SummarizeError::CostCapTooLow),
                // A failed reduce shouldn't waste the map work — fall back to the
                // first partial (a partial-but-real summary), flagged degraded.
                Err(_) => {
                    degraded = true;
                    SummaryOutcome {
                        summary: first,
                        model: model_name(),
                        cost_micros: guard.spent_micros(),
                        degraded,
                    }
                }
            }
        };
        // Graft the map-step citations (the reduce step's own citation indices
        // point at synthetic partials, so they're discarded).
        outcome.summary.citations = dedup_citations(carried);
        outcome.degraded = degraded;
        Ok(outcome)
    }

    /// Summarize one window of messages with the model ladder + cost guard:
    /// escalate on a recoverable quality problem, downgrade (flagged `degraded`)
    /// when the cap can't afford the current model. `length` picks the ladder
    /// start; `desired_output` is the output-token budget (smaller for map
    /// partials). The cost guard and `degraded` flag are threaded so a
    /// map-reduce run accounts for spend across all of its calls.
    #[allow(clippy::too_many_arguments)]
    fn summarize_window(
        &self,
        req: &SummarizeRequest,
        messages: &[&NormalizedMessage],
        length: SummaryLength,
        desired_output: i64,
        guard: &mut CostGuard,
        degraded: &mut bool,
    ) -> Result<SummaryOutcome, SummarizeError> {
        let message_ids: Vec<MessageId> = messages.iter().map(|m| m.id.clone()).collect();
        let prompt = assemble_prompt(messages, req.instructions);
        let input_tokens = estimate_tokens(&prompt);

        let mut current = self.ladder.start_index(length);
        // Bound the loop: at most one visit per ladder rung in each direction.
        let mut budget = self.ladder.len() * 2 + 1;

        loop {
            budget -= 1;
            if budget == 0 {
                return Err(SummarizeError::Llm(FailureClass::ServiceUnavailable));
            }
            let model = self.ladder.get(current).expect("current index in range");
            let alloc = allocate(
                input_tokens,
                model.context_tokens,
                desired_output,
                PROMPT_OVERHEAD_TOKENS,
            );
            let est = model
                .price
                .cost_micros(input_tokens, alloc.output_tokens_per_chunk);

            // Cost guard (Q#5): if this model won't fit, downgrade to a cheaper
            // one (flagged degraded); if there is none, the cap is too low.
            if guard.check(est) == SpendDecision::CapReached {
                if current > 0 {
                    current -= 1;
                    *degraded = true;
                    continue;
                }
                return Err(SummarizeError::CostCapTooLow);
            }

            let request = LlmRequest {
                provider: req.provider,
                priority: req.priority,
                model: model.name.clone(),
                prompt: prompt.clone(),
            };
            match self.engine.complete(&request) {
                Ok(response) => {
                    let out_tokens = estimate_tokens(&response.text);
                    guard.record(model.price.cost_micros(input_tokens, out_tokens));
                    match self.parse_and_finalize(
                        &response.text,
                        response.finish_reason,
                        &message_ids,
                    ) {
                        Ok(summary) => {
                            return Ok(SummaryOutcome {
                                summary,
                                model: response.model,
                                cost_micros: guard.spent_micros(),
                                degraded: *degraded,
                            })
                        }
                        // Recoverable quality problem: try a stronger model.
                        Err(q) => {
                            if current + 1 < self.ladder.len() {
                                current += 1;
                                continue;
                            }
                            return Err(SummarizeError::Quality(q));
                        }
                    }
                }
                Err(err) => match self.ladder.next_after_failure(current, err.class) {
                    NextModel::Escalate(next) => current = next,
                    NextModel::Retry(_) | NextModel::GiveUp => {
                        return Err(SummarizeError::Llm(err.class))
                    }
                },
            }
        }
    }

    /// Reduce map-step partials into a single summary. If the partials all fit
    /// one window, that's one final pass; otherwise they're reduced in groups to
    /// fewer partials and the process repeats — a multi-level reduce that needs
    /// no arbitrary cap on history size. Converges because each level produces
    /// strictly fewer partials.
    fn reduce(
        &self,
        req: &SummarizeRequest,
        mut partials: Vec<ExtractedSummary>,
        context_tokens: i64,
        guard: &mut CostGuard,
        degraded: &mut bool,
    ) -> Result<SummaryOutcome, SummarizeError> {
        let desired_output = output_budget(req.length);
        loop {
            let synthetic: Vec<NormalizedMessage> = partials
                .iter()
                .enumerate()
                .map(|(i, s)| synthetic_message(i, s))
                .collect();
            let refs: Vec<&NormalizedMessage> = synthetic.iter().collect();
            let tokens = estimate_tokens(&assemble_prompt(&refs, req.instructions));
            let plan = allocate(
                tokens,
                context_tokens,
                desired_output,
                PROMPT_OVERHEAD_TOKENS,
            );
            if plan.chunks <= 1 {
                return self.summarize_window(
                    req,
                    &refs,
                    req.length,
                    desired_output,
                    guard,
                    degraded,
                );
            }
            // Still too large: collapse to fewer partials, then loop.
            let mut next = Vec::with_capacity(plan.chunks as usize);
            for (lo, hi) in chunk_ranges(&refs, plan.input_tokens_per_chunk) {
                let out = self.summarize_window(
                    req,
                    &refs[lo..hi],
                    SummaryLength::Brief,
                    plan.output_tokens_per_chunk,
                    guard,
                    degraded,
                )?;
                next.push(out.summary);
            }
            partials = next;
        }
    }

    /// Drive the full job lifecycle (ADR-013) around a summary: record → start →
    /// summarize → complete/fail, persisting each transition. Returns the
    /// outcome (the structured summary is stored separately in Phase 4).
    pub fn summarize_as_job(
        &self,
        repo: &impl JobRepository,
        job_id: JobId,
        workspace: WorkspaceId,
        req: &SummarizeRequest,
        now: i64,
    ) -> anyhow::Result<Result<SummaryOutcome, SummarizeError>> {
        let mut job = Job::record(job_id, workspace, JobType::Summarization, now);
        repo.create_job(&job)?;
        job.start(now).map_err(|e| anyhow::anyhow!("{e:?}"))?;
        repo.update_job(&job)?;

        let result = self.summarize(req);
        match &result {
            Ok(outcome) => job
                .complete(outcome.cost_micros, now)
                .map_err(|e| anyhow::anyhow!("{e:?}"))?,
            Err(err) => job
                .fail(err.failure_class(), 0, now)
                .map_err(|e| anyhow::anyhow!("{e:?}"))?,
        }
        repo.update_job(&job)?;
        Ok(result)
    }

    fn parse_and_finalize(
        &self,
        json: &str,
        finish_reason: FinishReason,
        message_ids: &[MessageId],
    ) -> Result<ExtractedSummary, QualityError> {
        // A non-JSON / unparseable body is treated as an empty extraction, so it
        // falls into the same recoverable "try a stronger model" path. Real
        // models often wrap the JSON in ``` fences or prose — extract it first.
        let wire: WireExtraction = serde_json::from_str(extract_json(json)).unwrap_or_default();
        let raw = RawExtraction {
            text: wire.text,
            key_points: wire.key_points,
            action_items: wire
                .action_items
                .into_iter()
                .map(|a| ActionItem {
                    text: a.text,
                    assignee: a.assignee,
                })
                .collect(),
            technical_terms: wire.technical_terms,
            participants: wire.participants,
            citations: wire
                .citations
                .into_iter()
                .map(|c| RawCitation {
                    message_index: c.message_index,
                    quote: c.quote,
                })
                .collect(),
        };
        finalize(raw, finish_reason, message_ids)
    }
}

/// Pull the JSON object out of a model reply that may be wrapped in a ```` ``` ````
/// fence or prefixed with prose: prefer the inside of a fence, else the span
/// from the first `{` to the last `}`. Falls back to the trimmed input.
fn extract_json(body: &str) -> &str {
    let body = body.trim();
    let unfenced = match body.strip_prefix("```") {
        // Drop an optional language tag on the fence's first line, then the
        // closing fence.
        Some(rest) => {
            let after_tag = rest.split_once('\n').map(|x| x.1).unwrap_or(rest);
            after_tag.strip_suffix("```").unwrap_or(after_tag).trim()
        }
        None => body,
    };
    match (unfenced.find('{'), unfenced.rfind('}')) {
        (Some(start), Some(end)) if end >= start => &unfenced[start..=end],
        _ => unfenced,
    }
}

/// Rough token estimate: ~4 chars/token. The host swaps in a real tokenizer.
fn estimate_tokens(text: &str) -> i64 {
    (text.len() as i64).div_euclid(4).max(1)
}

/// Split messages into contiguous groups whose formatted input is each ≈
/// `budget_tokens`, preserving chronological order (the map step of map-reduce).
/// A single message larger than the budget forms its own group — a message can't
/// be split — so every group has at least one message.
fn chunk_ranges(messages: &[&NormalizedMessage], budget_tokens: i64) -> Vec<(usize, usize)> {
    let budget = budget_tokens.max(1);
    let mut ranges = Vec::new();
    let mut start = 0usize;
    let mut acc = 0i64;
    for (i, m) in messages.iter().enumerate() {
        let cost = estimate_tokens(&format!("[{i}] {}: {}\n", m.author_name, m.content));
        // Close the current group before this message would overflow it, but
        // never emit an empty group.
        if i > start && acc + cost > budget {
            ranges.push((start, i));
            start = i;
            acc = 0;
        }
        acc += cost;
    }
    ranges.push((start, messages.len()));
    ranges
}

/// Turn a map-step partial into a pseudo-message for the reduce step. The id is
/// synthetic (reduce citations are discarded in favor of the carried real ones),
/// and the content folds the partial's text + key points + action items so the
/// reducer sees the salient signal.
fn synthetic_message(index: usize, partial: &ExtractedSummary) -> NormalizedMessage {
    let mut content = partial.text.clone();
    if !partial.key_points.is_empty() {
        content.push_str("\nKey points: ");
        content.push_str(&partial.key_points.join("; "));
    }
    if !partial.action_items.is_empty() {
        let items: Vec<&str> = partial
            .action_items
            .iter()
            .map(|a| a.text.as_str())
            .collect();
        content.push_str("\nAction items: ");
        content.push_str(&items.join("; "));
    }
    NormalizedMessage {
        id: MessageId::parse(format!("partial-{index}")).expect("synthetic id is well-formed"),
        platform: Platform::Discord,
        channel_id: ChannelId::parse("reduce").expect("synthetic channel id is well-formed"),
        author_id: "summary".into(),
        author_name: format!("part {}", index + 1),
        content,
        timestamp: 0,
        is_system: false,
        reply_to: None,
        attachments: vec![],
    }
}

/// Dedup carried citations by (message id, quote), preserving first-seen order,
/// and cap the list so a very large history stays manageable.
fn dedup_citations(citations: Vec<ResolvedCitation>) -> Vec<ResolvedCitation> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for c in citations {
        let key = (c.message_id.as_str().to_string(), c.quote.clone());
        if seen.insert(key) {
            out.push(c);
            if out.len() >= MAX_CARRIED_CITATIONS {
                break;
            }
        }
    }
    out
}

fn output_budget(length: SummaryLength) -> i64 {
    match length {
        SummaryLength::Brief => 512,
        SummaryLength::Detailed => 1_024,
        SummaryLength::Comprehensive => 2_048,
    }
}

/// Build the summarization prompt: the numbered messages first (the `[i]` prefix
/// is the citation index), then the instruction + the exact JSON schema we parse.
///
/// The instruction goes **last** deliberately. A local Ollama served over its
/// OpenAI-compatible `/v1` endpoint ignores `options.num_ctx` and silently
/// truncates a prompt that exceeds its (small, ~4k) default context — dropping
/// the *oldest* tokens. With the instruction at the end it survives truncation;
/// only the earliest messages are lost, which is a graceful degradation. The
/// deterministic demo client ignores the instruction (it only reads `[i]` lines);
/// a real model needs it to produce the structured output.
fn assemble_prompt(messages: &[&NormalizedMessage], instructions: Option<&str>) -> String {
    let mut s = String::from("Numbered chat messages:\n");
    for (i, m) in messages.iter().enumerate() {
        s.push_str(&format!("[{i}] {}: {}\n", m.author_name, m.content));
    }
    s.push_str(
        "\nYou are a summarization engine. Read the numbered chat messages above and \
         reply with ONLY a single minified JSON object — no prose, no markdown code \
         fences — of exactly this shape:\n\
         {\"text\":\"<concise prose summary>\",\"key_points\":[\"...\"],\
         \"action_items\":[{\"text\":\"...\",\"assignee\":null}],\
         \"technical_terms\":[\"...\"],\"participants\":[\"...\"],\
         \"citations\":[{\"message_index\":0,\"quote\":\"<verbatim snippet>\"}]}\n\
         Set message_index from a message's [index] prefix. Use empty arrays for \
         anything you can't fill.",
    );
    // Per-workspace guidance (SUM-007), last so it's prominent and survives any
    // context-window truncation of the messages above.
    if let Some(extra) = instructions.map(str::trim).filter(|e| !e.is_empty()) {
        s.push_str("\nAdditional instructions: ");
        s.push_str(extra);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{GlobalRateLimiter, LlmError, LlmResponse, RateLimitConfig};
    use domain::summarize::{Model, ModelPrice};
    use domain::{ChannelId, Platform};
    use repository::SqliteRepository;
    use std::cell::RefCell;
    use std::sync::Arc;

    struct ScriptedClient {
        script: RefCell<Vec<Result<LlmResponse, LlmError>>>,
    }
    impl LlmClient for ScriptedClient {
        fn complete(&self, _req: &LlmRequest) -> Result<LlmResponse, LlmError> {
            self.script.borrow_mut().remove(0)
        }
    }

    fn resp(json: &str, finish: FinishReason) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            model: "test-model".into(),
            text: json.into(),
            finish_reason: finish,
        })
    }

    fn price(i: i64, o: i64) -> ModelPrice {
        ModelPrice {
            input_micros_per_ktoken: i,
            output_micros_per_ktoken: o,
        }
    }

    fn ladder() -> ModelLadder {
        ModelLadder::new(vec![
            Model {
                name: "haiku".into(),
                price: price(800, 4_000),
                context_tokens: 200_000,
            },
            Model {
                name: "sonnet".into(),
                price: price(3_000, 15_000),
                context_tokens: 200_000,
            },
            Model {
                name: "opus".into(),
                price: price(15_000, 75_000),
                context_tokens: 200_000,
            },
        ])
    }

    fn engine(script: Vec<Result<LlmResponse, LlmError>>) -> ResilientLlm<ScriptedClient> {
        let limiter = Arc::new(GlobalRateLimiter::new(RateLimitConfig::default()));
        ResilientLlm::new(
            ScriptedClient {
                script: RefCell::new(script),
            },
            limiter,
        )
    }

    fn msg(id: &str, content: &str) -> NormalizedMessage {
        NormalizedMessage {
            id: MessageId::parse(id).unwrap(),
            platform: Platform::Discord,
            channel_id: ChannelId::parse("c1").unwrap(),
            author_id: "u1".into(),
            author_name: "Alice".into(),
            content: content.into(),
            timestamp: 0,
            is_system: false,
            reply_to: None,
            attachments: vec![],
        }
    }

    fn messages() -> Vec<NormalizedMessage> {
        vec![
            msg("m0", "We should ship the launch on Friday"),
            msg("m1", "Agreed, I'll prepare the changelog"),
            msg("m2", "ok"), // trivial — filtered out
        ]
    }

    const GOOD_JSON: &str = r#"{
        "text": "The team agreed to launch Friday.",
        "key_points": ["Launch on Friday"],
        "action_items": [{"text": "prepare changelog", "assignee": "Alice"}],
        "technical_terms": [],
        "participants": ["Alice"],
        "citations": [{"message_index": 0, "quote": "ship the launch"}]
    }"#;

    fn req<'a>(messages: &'a [NormalizedMessage], cap: i64) -> SummarizeRequest<'a> {
        SummarizeRequest {
            messages,
            length: SummaryLength::Detailed,
            provider: LlmProvider::OpenRouter,
            priority: RequestPriority::Manual,
            cap_micros: cap,
            instructions: None,
        }
    }

    #[test]
    fn assemble_prompt_appends_workspace_instructions() {
        let msgs = messages();
        let refs: Vec<&NormalizedMessage> = msgs.iter().collect();
        let base = assemble_prompt(&refs, None);
        assert!(!base.contains("Additional instructions"));
        let custom = assemble_prompt(&refs, Some("Focus on decisions and risks."));
        assert!(custom.contains("Additional instructions: Focus on decisions and risks."));
        // Empty/whitespace guidance is ignored.
        assert_eq!(assemble_prompt(&refs, Some("   ")), base);
    }

    #[test]
    fn produces_validated_summary_with_resolved_citations() {
        let l = ladder();
        let e = engine(vec![resp(GOOD_JSON, FinishReason::Stop)]);
        let svc = SummarizationService::new(&e, &l);
        let msgs = messages();
        let out = svc.summarize(&req(&msgs, i64::MAX)).unwrap();
        assert_eq!(out.summary.key_points, vec!["Launch on Friday".to_string()]);
        assert_eq!(out.summary.citations[0].message_id.as_str(), "m0");
        assert!(!out.degraded);
        assert!(out.cost_micros > 0);
    }

    #[test]
    fn empty_input_after_filtering_is_rejected() {
        let l = ladder();
        let e = engine(vec![]);
        let svc = SummarizationService::new(&e, &l);
        let only_trivial = vec![msg("m0", "ok"), msg("m1", "👍")];
        assert_eq!(
            svc.summarize(&req(&only_trivial, i64::MAX)),
            Err(SummarizeError::NoSubstantialMessages)
        );
    }

    #[test]
    fn truncated_output_escalates_to_a_stronger_model() {
        let l = ladder();
        // Detailed starts at index 1 (sonnet): truncated → escalate to opus → ok.
        let e = engine(vec![
            resp(GOOD_JSON, FinishReason::Length),
            resp(GOOD_JSON, FinishReason::Stop),
        ]);
        let svc = SummarizationService::new(&e, &l);
        let msgs = messages();
        let out = svc.summarize(&req(&msgs, i64::MAX)).unwrap();
        assert_eq!(out.model, "test-model");
    }

    #[test]
    fn cost_cap_downgrades_to_cheaper_model_flagged_degraded() {
        let l = ladder();
        // Detailed starts at sonnet (~15k µ$ here); a cap that only fits haiku
        // (~4k µ$) forces a downgrade flagged degraded (Q#5).
        let e = engine(vec![resp(GOOD_JSON, FinishReason::Stop)]);
        let svc = SummarizationService::new(&e, &l);
        let msgs = messages();
        let out = svc.summarize(&req(&msgs, 10_000)).unwrap();
        assert!(out.degraded);
        assert_eq!(out.summary.key_points, vec!["Launch on Friday".to_string()]);
    }

    #[test]
    fn permanent_llm_failure_gives_up_with_class() {
        let l = ladder();
        let e = engine(vec![Err(LlmError {
            class: FailureClass::InvalidRequest,
            retry_after_secs: None,
            detail: "bad".into(),
        })]);
        let svc = SummarizationService::new(&e, &l);
        let msgs = messages();
        assert_eq!(
            svc.summarize(&req(&msgs, i64::MAX)),
            Err(SummarizeError::Llm(FailureClass::InvalidRequest))
        );
    }

    #[test]
    fn job_lifecycle_completes_on_success() {
        let l = ladder();
        let e = engine(vec![resp(GOOD_JSON, FinishReason::Stop)]);
        let svc = SummarizationService::new(&e, &l);
        let repo = SqliteRepository::in_memory().unwrap();
        let msgs = messages();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        let id = JobId::parse("job_1").unwrap();

        let result = svc
            .summarize_as_job(&repo, id.clone(), ws.clone(), &req(&msgs, i64::MAX), 1_000)
            .unwrap();
        assert!(result.is_ok());
        let job = repo.get_job(&ws, &id).unwrap().unwrap();
        assert_eq!(job.status, domain::JobStatus::Completed);
        assert!(job.cost_micros > 0);
    }

    #[test]
    fn job_lifecycle_fails_with_classified_reason() {
        let l = ladder();
        let e = engine(vec![Err(LlmError {
            class: FailureClass::InvalidRequest,
            retry_after_secs: None,
            detail: "bad".into(),
        })]);
        let svc = SummarizationService::new(&e, &l);
        let repo = SqliteRepository::in_memory().unwrap();
        let msgs = messages();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        let id = JobId::parse("job_2").unwrap();

        let result = svc
            .summarize_as_job(&repo, id.clone(), ws.clone(), &req(&msgs, i64::MAX), 1_000)
            .unwrap();
        assert!(result.is_err());
        let job = repo.get_job(&ws, &id).unwrap().unwrap();
        assert_eq!(job.status, domain::JobStatus::Failed);
        assert_eq!(job.failure_reason.as_deref(), Some("invalid_request"));
    }

    #[test]
    fn extract_json_handles_fences_and_prose() {
        let obj = r#"{"text":"hi"}"#;
        // Bare object.
        assert_eq!(extract_json(obj), obj);
        // Fenced with a language tag.
        assert_eq!(extract_json("```json\n{\"text\":\"hi\"}\n```"), obj);
        // Prose before/after.
        assert_eq!(
            extract_json("Here is the summary:\n{\"text\":\"hi\"} — done"),
            obj
        );
        // Fenced without a tag.
        assert_eq!(extract_json("```\n{\"text\":\"hi\"}\n```"), obj);
    }

    /// A single-model ladder with a constrained context window — forces
    /// map-reduce for transcripts larger than the window.
    fn small_ctx_ladder(context_tokens: i64) -> ModelLadder {
        ModelLadder::new(vec![Model {
            name: "local".into(),
            price: price(0, 0),
            context_tokens,
        }])
    }

    fn many_oks(n: usize) -> Vec<Result<LlmResponse, LlmError>> {
        (0..n)
            .map(|_| resp(GOOD_JSON, FinishReason::Stop))
            .collect()
    }

    fn long_messages(count: usize) -> Vec<NormalizedMessage> {
        (0..count)
            .map(|i| {
                msg(
                    &format!("m{i}"),
                    &"we debated the plan and agreed on the concrete next steps to take ".repeat(6),
                )
            })
            .collect()
    }

    #[test]
    fn long_transcript_is_map_reduced_and_carries_real_citations() {
        // A tiny window forces several map chunks plus a reduce pass; plenty of
        // scripted OKs cover every map + reduce call.
        let l = small_ctx_ladder(2_000);
        let e = engine(many_oks(40));
        let svc = SummarizationService::new(&e, &l);
        let msgs = long_messages(12);
        let out = svc.summarize(&req(&msgs, i64::MAX)).unwrap();

        assert!(!out.summary.text.is_empty());
        // The reduce step only sees synthetic partials, so its own citation
        // indices are discarded — the surfaced citations are the map step's,
        // grounded to the original `m*` message ids (never `partial-*`).
        assert!(!out.summary.citations.is_empty());
        assert!(out
            .summary
            .citations
            .iter()
            .all(|c| c.message_id.as_str().starts_with('m')));
    }

    #[test]
    fn map_reduce_tolerates_a_failed_chunk_and_flags_degraded() {
        // First map chunk returns an empty extraction (a flaky reply); the rest
        // succeed. The history still summarizes, flagged degraded.
        let l = small_ctx_ladder(2_000);
        let empty = r#"{"text":"","key_points":[]}"#;
        let script = std::iter::once(resp(empty, FinishReason::Stop))
            .chain(many_oks(40))
            .collect();
        let e = engine(script);
        let svc = SummarizationService::new(&e, &l);
        let msgs = long_messages(12);
        let out = svc.summarize(&req(&msgs, i64::MAX)).unwrap();
        assert!(out.degraded, "a dropped chunk must degrade the result");
        assert!(!out.summary.text.is_empty());
    }

    #[test]
    fn single_pass_when_transcript_fits_the_window() {
        // A roomy window summarizes in one call (chunks == 1): exactly one OK.
        let l = small_ctx_ladder(200_000);
        let e = engine(many_oks(1));
        let svc = SummarizationService::new(&e, &l);
        let msgs = messages();
        let out = svc.summarize(&req(&msgs, i64::MAX)).unwrap();
        assert_eq!(out.summary.citations[0].message_id.as_str(), "m0");
    }

    #[test]
    fn chunk_ranges_are_contiguous_nonempty_and_cover_everything() {
        let msgs = long_messages(7);
        let refs: Vec<&NormalizedMessage> = msgs.iter().collect();
        let ranges = chunk_ranges(&refs, 30); // tiny budget → many small groups
        assert!(ranges.len() > 1);
        assert_eq!(ranges.first().unwrap().0, 0);
        assert_eq!(ranges.last().unwrap().1, refs.len());
        for (a, b) in &ranges {
            assert!(b > a, "no group may be empty");
        }
        for w in ranges.windows(2) {
            assert_eq!(w[0].1, w[1].0, "groups must be contiguous");
        }
    }

    #[test]
    fn oversized_single_message_forms_its_own_chunk() {
        let msgs = long_messages(1);
        let refs: Vec<&NormalizedMessage> = msgs.iter().collect();
        // Budget far smaller than the one message — it can't be split.
        let ranges = chunk_ranges(&refs, 1);
        assert_eq!(ranges, vec![(0, 1)]);
    }

    #[test]
    fn dedup_citations_drops_duplicates_and_caps_length() {
        let cite = |id: &str, q: &str| ResolvedCitation {
            message_id: MessageId::parse(id).unwrap(),
            quote: Some(q.to_string()),
        };
        let out = dedup_citations(vec![cite("m0", "a"), cite("m0", "a"), cite("m1", "b")]);
        assert_eq!(out.len(), 2);

        let many: Vec<ResolvedCitation> = (0..MAX_CARRIED_CITATIONS + 10)
            .map(|i| cite(&format!("m{i}"), "q"))
            .collect();
        assert_eq!(dedup_citations(many).len(), MAX_CARRIED_CITATIONS);
    }
}
