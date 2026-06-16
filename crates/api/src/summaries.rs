//! Summary management endpoints (PRD §5.1): list / detail / pin / archive / tag.
//! Workspace-scoped and auth-gated; thin over `StructuredSummaryRepository`.

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use domain::summarize::SummaryLength;
use repository::{StructuredSummaryRepository, SummaryQuery, SummaryRecord};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// JSON shape of a stored summary (domain types have no serde, so map here).
#[derive(Serialize)]
pub struct SummaryDto {
    pub id: String,
    pub channel_id: Option<String>,
    pub model: String,
    pub cost_micros: i64,
    pub degraded: bool,
    /// Coherence-gate grounded score in `[0,1]` (COH-001); `null` if unassessed.
    pub coherence_score: Option<f32>,
    /// Latency of the producing run, in milliseconds (ADR-106 metadata).
    pub latency_ms: i64,
    /// Input/output tokens the producing run consumed (ADR-106 metadata).
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// The message-time window this summary covers (ADR-133). Zero-width
    /// (`start == end`) for ad-hoc pasted text. Drives the Regenerate affordance.
    pub period_start: i64,
    pub period_end: i64,
    /// Built-in perspective that steered this summary (ADR-133 §B), if any.
    pub perspective: Option<String>,
    pub pinned: bool,
    pub archived: bool,
    pub tags: Vec<String>,
    pub created_at: i64,
    pub text: String,
    pub key_points: Vec<KeyPointDto>,
    pub action_items: Vec<ActionItemDto>,
    pub technical_terms: Vec<String>,
    pub participants: Vec<String>,
    pub citations: Vec<CitationDto>,
}

/// A grounded key point (ADR-004): the claim text, its self-assessed confidence,
/// and the source-message references that support it.
#[derive(Serialize)]
pub struct KeyPointDto {
    pub text: String,
    pub confidence: f32,
    pub references: Vec<ReferenceDto>,
}

/// One source reference behind a claim (ADR-004 §2.1).
#[derive(Serialize)]
pub struct ReferenceDto {
    pub message_id: String,
    pub author_name: String,
    pub timestamp: i64,
    pub position: usize,
    pub snippet: String,
}

#[derive(Serialize)]
pub struct ActionItemDto {
    pub text: String,
    pub assignee: Option<String>,
}

#[derive(Serialize)]
pub struct CitationDto {
    pub message_id: String,
    pub quote: Option<String>,
}

impl From<SummaryRecord> for SummaryDto {
    fn from(r: SummaryRecord) -> Self {
        SummaryDto {
            id: r.id,
            channel_id: r.channel_id.map(|c| c.as_str().to_string()),
            model: r.model,
            cost_micros: r.cost_micros,
            degraded: r.degraded,
            coherence_score: r.coherence_score,
            latency_ms: r.usage.latency_ms,
            input_tokens: r.usage.input_tokens,
            output_tokens: r.usage.output_tokens,
            period_start: r.period_start,
            period_end: r.period_end,
            perspective: r.perspective,
            pinned: r.pinned,
            archived: r.archived,
            tags: r.tags,
            created_at: r.created_at,
            text: r.summary.text,
            key_points: r
                .summary
                .key_points
                .into_iter()
                .map(|k| KeyPointDto {
                    text: k.text,
                    confidence: k.confidence,
                    references: k
                        .references
                        .into_iter()
                        .map(|rf| ReferenceDto {
                            message_id: rf.message_id.as_str().to_string(),
                            author_name: rf.author_name,
                            timestamp: rf.timestamp,
                            position: rf.position,
                            snippet: rf.snippet,
                        })
                        .collect(),
                })
                .collect(),
            action_items: r
                .summary
                .action_items
                .into_iter()
                .map(|a| ActionItemDto {
                    text: a.text,
                    assignee: a.assignee,
                })
                .collect(),
            technical_terms: r.summary.technical_terms,
            participants: r.summary.participants,
            citations: r
                .summary
                .citations
                .into_iter()
                .map(|c| CitationDto {
                    message_id: c.message_id.as_str().to_string(),
                    quote: c.quote,
                })
                .collect(),
        }
    }
}

/// `?include_archived=true&limit=50&offset=0&q=roadmap&participant=Bob&tag=ops`
/// — filtering + pagination for the dashboard listing (DSH-002/004/005).
#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub include_archived: bool,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
    /// Free-text substring over summary text + key points (DSH-004).
    #[serde(default)]
    pub q: Option<String>,
    /// Substring over the participant list (DSH-005).
    #[serde(default)]
    pub participant: Option<String>,
    /// Substring over the tag list (DSH-009).
    #[serde(default)]
    pub tag: Option<String>,
}

fn default_limit() -> u32 {
    50
}

/// Treat a blank/whitespace-only query param as absent.
fn non_blank(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Body for on-demand summarize: a batch of message texts (optionally authored).
#[derive(Deserialize)]
pub struct CreateSummaryRequest {
    pub messages: Vec<String>,
    /// Display author for every message (the demo doesn't model per-message
    /// authorship); defaults to "user".
    #[serde(default = "default_author")]
    pub author: String,
    /// Optional built-in perspective id (ADR-133), e.g. "developer".
    #[serde(default)]
    pub perspective: Option<String>,
    /// Optional saved prompt-template id (ADR-133). Takes precedence over
    /// `perspective`; its content is prepended to the workspace instructions.
    #[serde(default)]
    pub prompt_template_id: Option<String>,
}

fn default_author() -> String {
    "user".to_string()
}

/// `POST /workspaces/:ws/summaries` — on-demand summarize through the **real
/// Phase-3 pipeline** (`SummarizationService` → model ladder → structured
/// extraction + quality validation), over the **process-wide shared** LLM
/// backend and rate limiter held in [`AppState`]. That backend is the
/// deterministic demo client by default (no key/network) and the OpenRouter
/// client when the server is configured for it (`main.rs`); this handler is
/// unchanged either way.
pub async fn create_summary(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Json(body): Json<CreateSummaryRequest>,
) -> Result<Json<SummaryDto>, ApiError> {
    use host::llm::{LlmProvider, RequestPriority, ResilientLlm};
    use host::{SummarizationService, SummarizeRequest};

    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    if body.messages.is_empty() {
        return Err(ApiError::bad_request("messages must not be empty"));
    }
    let now = crate::auth::now_secs();

    // Wrap the raw texts as normalized messages for the pipeline.
    let channel = domain::ChannelId::parse("on-demand").expect("valid channel id");
    let messages: Vec<domain::NormalizedMessage> = body
        .messages
        .iter()
        .enumerate()
        .map(|(i, text)| domain::NormalizedMessage {
            id: domain::MessageId::parse(format!("od_{now}_{i}")).expect("valid id"),
            platform: domain::Platform::Discord,
            channel_id: channel.clone(),
            author_id: body.author.clone(),
            author_name: body.author.clone(),
            content: text.clone(),
            timestamp: now,
            is_system: false,
            reply_to: None,
            attachments: vec![],
        })
        .collect();

    // Resolve any per-tenant LLM override for this workspace (ADR-125), gate on
    // the tenant's budget, then build the engine over that backend + the shared
    // limiter.
    let (resolution, charge, instructions) = {
        use repository::{PromptTemplateRepository, WorkspaceSettingsRepository};
        let repo = state.repo.lock().expect("repo mutex");
        let r = crate::resolve_llm(&repo, &workspace, &state.model, state.master_key());
        let charge = crate::budget_gate(&repo, &r, now)?;
        let base = repo
            .get_settings(&workspace)
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .summary_instructions;
        // Steering (ADR-133): a saved template (takes precedence, bumps its usage)
        // or a built-in perspective prepends its instructions to the workspace's.
        let steer: Option<String> = if let Some(id) = &body.prompt_template_id {
            let t = repo
                .get_template(&workspace, id)
                .map_err(|e| ApiError::Internal(e.to_string()))?
                .ok_or_else(|| ApiError::bad_request("unknown prompt_template_id"))?;
            let _ = repo.bump_template_usage(&workspace, id);
            Some(t.content)
        } else if let Some(p) = &body.perspective {
            domain::summarize::Perspective::parse(p)
                .ok_or_else(|| ApiError::bad_request("unknown perspective"))?
                .instructions()
                .map(|s| s.to_string())
        } else {
            None
        };
        let instructions = match (steer, base) {
            (Some(s), Some(b)) => Some(format!("{s}\n\n{b}")),
            (Some(s), None) => Some(s),
            (None, b) => b,
        };
        (r, charge, instructions)
    };
    let ladder = state.ladder_for(&resolution.model);
    let engine = ResilientLlm::new(
        state.client_for_base(resolution.base_url, resolution.api_key),
        state.limiter.clone(),
    );
    // Track the on-demand summary as a job (ADR-013/040) so it shows in the Jobs
    // view with status + cost. Recorded (Running) after the budget gate, just
    // before the LLM work; finalized to Completed/Failed below.
    let pstart = messages.iter().map(|m| m.timestamp).min().unwrap_or(now);
    let pend = messages.iter().map(|m| m.timestamp).max().unwrap_or(now);
    let mut job = domain::Job::record(
        domain::JobId::parse(format!("job_sum_{}", unique_suffix()))
            .map_err(|e| ApiError::bad_request(e.to_string()))?,
        workspace.clone(),
        domain::JobType::Summarization,
        now,
    )
    .with_scope("on-demand")
    .with_creation_source("manual")
    .with_date_range(pstart, pend);
    let _ = job.start(now);
    {
        use repository::JobRepository;
        let repo = state.repo.lock().expect("repo mutex");
        let _ = repo.create_job(&job);
    }

    let outcome = match SummarizationService::new(&engine, &ladder).summarize(&SummarizeRequest {
        messages: &messages,
        length: SummaryLength::Detailed,
        provider: LlmProvider::OpenRouter,
        priority: RequestPriority::Manual,
        cap_micros: i64::MAX,
        instructions: instructions.as_deref(),
    }) {
        Ok(o) => o,
        Err(e) => {
            use repository::JobRepository;
            let _ = job.fail(e.failure_class(), 0, crate::auth::now_secs());
            let repo = state.repo.lock().expect("repo mutex");
            let _ = repo.update_job(&job);
            // Record the operational failure for the Errors view (ADR-133/031).
            crate::errors::record_operational_error(
                &repo,
                &workspace,
                "summarize",
                e.failure_class(),
                None,
                format!("on-demand summary failed: {e:?}"),
            );
            return Err(ApiError::bad_request(format!("{e:?}")));
        }
    };

    let record = SummaryRecord {
        id: format!("sum_{}", unique_suffix()),
        channel_id: Some(channel),
        model: outcome.model,
        cost_micros: outcome.cost_micros,
        degraded: outcome.degraded,
        created_at: now,
        pinned: false,
        archived: false,
        tags: vec![],
        coherence_score: Some(outcome.coherence.score),
        usage: outcome.usage,
        // The covered window = span of the input messages (ADR-133 coverage).
        // On-demand pasted text is stamped `now`, so this collapses to a point.
        period_start: messages.iter().map(|m| m.timestamp).min().unwrap_or(now),
        period_end: messages.iter().map(|m| m.timestamp).max().unwrap_or(now),
        perspective: body.perspective.clone(),
        summary: outcome.summary,
    };
    {
        let repo = state.repo.lock().expect("repo mutex");
        // Always-on dashboard store + any configured external destinations
        // (DSH-010/011), gated by the workspace's delivery capabilities.
        let deliverers = state.deliverers();
        let (destinations, caps) =
            host::load_workspace_delivery(&*repo, &workspace, state.master_key())
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        host::DeliveryService::new(&*repo)
            .with_deliverers(&deliverers)
            .deliver(&workspace, &record, &destinations, &caps)
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        // Draw down the tenant's budget by what this call cost (ADR-125 Phase 3).
        if let Some((tenant, window)) = &charge {
            crate::budget_charge(&repo, tenant, *window, record.cost_micros)?;
        }
        // Extract + embed knowledge units (ADR-127); best-effort.
        crate::knowledge::ingest_summary(
            &state,
            &repo,
            &workspace,
            &record.summary,
            &record.id,
            record.channel_id.as_ref().map(|c| c.as_str()),
            now,
        );
        // Finalize the job with the cost the summary actually incurred.
        use repository::JobRepository;
        job.add_summary_id(record.id.clone());
        let _ = job.complete(record.cost_micros, crate::auth::now_secs());
        let _ = repo.update_job(&job);
    }
    state.publish(crate::LiveEvent::summary_created(&workspace, &record.id));
    Ok(Json(SummaryDto::from(record)))
}

/// Body for `regenerate_summary` — all optional (defaults reuse the original).
#[derive(Deserialize)]
pub struct RegenerateRequest {
    #[serde(default)]
    pub perspective: Option<String>,
    #[serde(default)]
    pub prompt_template_id: Option<String>,
    /// `brief` | `detailed` | `comprehensive`; defaults to detailed.
    #[serde(default)]
    pub length: Option<String>,
}

/// `POST /workspaces/:ws/summaries/:id/regenerate` (ADR-133) — re-run a stored
/// summary over its *same source window* with changed params (perspective /
/// template / length), producing a new summary. Tracked as a `Regenerate` job.
/// Only works for a channel-scoped summary that recorded a real window.
pub async fn regenerate_summary(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
    Json(body): Json<RegenerateRequest>,
) -> Result<Json<SummaryDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let record = regenerate_core(&state, &workspace, &id, &body).await?;
    Ok(Json(SummaryDto::from(record)))
}

/// Core of a single regenerate (ADR-133): re-run summary `id` over its recorded
/// window with the request's steering, deliver + ingest, and return the new
/// record. Shared by the single endpoint and bulk regenerate (D3).
async fn regenerate_core(
    state: &AppState,
    workspace: &domain::WorkspaceId,
    id: &str,
    body: &RegenerateRequest,
) -> Result<SummaryRecord, ApiError> {
    use host::llm::{LlmProvider, RequestPriority, ResilientLlm};
    use host::{SummarizationService, SummarizeRequest};
    use repository::{JobRepository, PromptTemplateRepository, WhatsAppRepository};

    let workspace = workspace.clone();
    let now = crate::auth::now_secs();

    // Load the original + its recorded source window.
    let original = {
        let repo = state.repo.lock().expect("repo mutex");
        repo.get_record(&workspace, &id)
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or(ApiError::NotFound)?
    };
    let Some(channel) = original.channel_id.clone() else {
        return Err(ApiError::bad_request(
            "this summary spans multiple channels or has no source window — cannot regenerate",
        ));
    };
    if original.period_end <= original.period_start {
        return Err(ApiError::bad_request(
            "this summary has no stored source window to regenerate from",
        ));
    }
    let length = match body.length.as_deref() {
        Some("brief") => SummaryLength::Brief,
        Some("comprehensive") => SummaryLength::Comprehensive,
        Some("detailed") | None => SummaryLength::Detailed,
        Some(_) => {
            return Err(ApiError::bad_request("length must be brief|detailed|comprehensive"))
        }
    };

    // Re-read the messages that fell in the original window.
    let messages = {
        let repo = state.repo.lock().expect("repo mutex");
        repo.list_messages(&workspace, &channel, original.period_start, original.period_end)
            .map_err(|e| ApiError::Internal(e.to_string()))?
    };
    if !messages.iter().any(|m| m.is_substantial()) {
        return Err(ApiError::bad_request(
            "no substantial messages remain in the original window",
        ));
    }

    // Resolve LLM + budget + steering (perspective/template), as create_summary.
    let (resolution, charge, instructions) = {
        use repository::WorkspaceSettingsRepository;
        let repo = state.repo.lock().expect("repo mutex");
        let r = crate::resolve_llm(&repo, &workspace, &state.model, state.master_key());
        let charge = crate::budget_gate(&repo, &r, now)?;
        let base = repo
            .get_settings(&workspace)
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .summary_instructions;
        let steer: Option<String> = if let Some(tid) = &body.prompt_template_id {
            let t = repo
                .get_template(&workspace, tid)
                .map_err(|e| ApiError::Internal(e.to_string()))?
                .ok_or_else(|| ApiError::bad_request("unknown prompt_template_id"))?;
            let _ = repo.bump_template_usage(&workspace, tid);
            Some(t.content)
        } else if let Some(p) = &body.perspective {
            domain::summarize::Perspective::parse(p)
                .ok_or_else(|| ApiError::bad_request("unknown perspective"))?
                .instructions()
                .map(|s| s.to_string())
        } else {
            None
        };
        let instructions = match (steer, base) {
            (Some(s), Some(b)) => Some(format!("{s}\n\n{b}")),
            (Some(s), None) => Some(s),
            (None, b) => b,
        };
        (r, charge, instructions)
    };
    let ladder = state.ladder_for(&resolution.model);
    let engine = ResilientLlm::new(
        state.client_for_base(resolution.base_url, resolution.api_key),
        state.limiter.clone(),
    );

    // Track as a Regenerate job (ADR-013/040/133).
    let mut job = domain::Job::record(
        domain::JobId::parse(format!("job_regen_{}", unique_suffix()))
            .map_err(|e| ApiError::bad_request(e.to_string()))?,
        workspace.clone(),
        domain::JobType::Regenerate,
        now,
    )
    .with_scope(format!("channel #{}", channel.as_str()))
    .with_creation_source("manual")
    .with_date_range(original.period_start, original.period_end);
    let _ = job.start(now);
    {
        let repo = state.repo.lock().expect("repo mutex");
        let _ = repo.create_job(&job);
    }

    let outcome = match SummarizationService::new(&engine, &ladder).summarize(&SummarizeRequest {
        messages: &messages,
        length,
        provider: LlmProvider::OpenRouter,
        priority: RequestPriority::Manual,
        cap_micros: i64::MAX,
        instructions: instructions.as_deref(),
    }) {
        Ok(o) => o,
        Err(e) => {
            let _ = job.fail(e.failure_class(), 0, crate::auth::now_secs());
            let repo = state.repo.lock().expect("repo mutex");
            let _ = repo.update_job(&job);
            return Err(ApiError::bad_request(format!("{e:?}")));
        }
    };

    let record = SummaryRecord {
        id: format!("sum_regen_{}", unique_suffix()),
        channel_id: Some(channel),
        model: outcome.model,
        cost_micros: outcome.cost_micros,
        degraded: outcome.degraded,
        created_at: now,
        pinned: false,
        archived: false,
        tags: vec![format!("regenerated-from:{id}")],
        coherence_score: Some(outcome.coherence.score),
        usage: outcome.usage,
        // Same window the original covered.
        period_start: original.period_start,
        period_end: original.period_end,
        perspective: body.perspective.clone(),
        summary: outcome.summary,
    };
    {
        let repo = state.repo.lock().expect("repo mutex");
        let deliverers = state.deliverers();
        let (destinations, caps) =
            host::load_workspace_delivery(&*repo, &workspace, state.master_key())
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        host::DeliveryService::new(&*repo)
            .with_deliverers(&deliverers)
            .deliver(&workspace, &record, &destinations, &caps)
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        if let Some((tenant, window)) = &charge {
            crate::budget_charge(&repo, tenant, *window, record.cost_micros)?;
        }
        crate::knowledge::ingest_summary(
            &state,
            &repo,
            &workspace,
            &record.summary,
            &record.id,
            record.channel_id.as_ref().map(|c| c.as_str()),
            now,
        );
        job.add_summary_id(record.id.clone());
        let _ = job.complete(record.cost_micros, crate::auth::now_secs());
        let _ = repo.update_job(&job);
    }
    state.publish(crate::LiveEvent::summary_created(&workspace, &record.id));
    Ok(record)
}

/// `POST /workspaces/:ws/summaries/regenerate` — bulk regenerate (ADR-133 D3).
/// Each id is regenerated independently with the shared steering; non-regenerable
/// summaries (no window/channel) are counted as skipped rather than failing all.
pub async fn bulk_regenerate(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Json(body): Json<BulkRegenerateRequest>,
) -> Result<Json<BulkRegenerateResult>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let opts = RegenerateRequest {
        perspective: body.perspective.clone(),
        prompt_template_id: body.prompt_template_id.clone(),
        length: body.length.clone(),
    };
    let (mut regenerated, mut skipped) = (0usize, 0usize);
    let mut new_ids = Vec::new();
    for id in &body.ids {
        match regenerate_core(&state, &workspace, id, &opts).await {
            Ok(rec) => {
                regenerated += 1;
                new_ids.push(rec.id);
            }
            Err(_) => skipped += 1,
        }
    }
    Ok(Json(BulkRegenerateResult { regenerated, skipped, new_ids }))
}

#[derive(Deserialize)]
pub struct BulkRegenerateRequest {
    pub ids: Vec<String>,
    #[serde(default)]
    pub perspective: Option<String>,
    #[serde(default)]
    pub prompt_template_id: Option<String>,
    #[serde(default)]
    pub length: Option<String>,
}

#[derive(Serialize)]
pub struct BulkRegenerateResult {
    pub regenerated: usize,
    pub skipped: usize,
    pub new_ids: Vec<String>,
}

/// A unique-enough id suffix from the clock (nanos).
fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Per-model spend row in the cost dashboard.
#[derive(Serialize)]
pub struct ModelSpendDto {
    pub model: String,
    pub count: i64,
    pub cost_micros: i64,
}

/// Cost analytics for a workspace (ADR-125): spend rolled up from stored
/// summaries. Money is micro-dollars (1 USD = 1_000_000 µ$).
#[derive(Serialize)]
pub struct SpendDto {
    pub total_micros: i64,
    pub summary_count: i64,
    pub recent_micros: i64,
    /// Window (days) `recent_micros` covers.
    pub recent_days: i64,
    pub by_model: Vec<ModelSpendDto>,
}

/// `GET /workspaces/:ws/spend?days=30` — summarization cost analytics.
pub async fn spend(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Query(q): Query<SpendQuery>,
) -> Result<Json<SpendDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let days = q.days.unwrap_or(30).clamp(1, 365);
    let since = crate::auth::now_secs() - days * 86_400;
    let repo = state.repo.lock().expect("repo mutex");
    let breakdown = repo
        .workspace_spend(&workspace, since)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(SpendDto {
        total_micros: breakdown.total_micros,
        summary_count: breakdown.summary_count,
        recent_micros: breakdown.recent_micros,
        recent_days: days,
        by_model: breakdown
            .by_model
            .into_iter()
            .map(|m| ModelSpendDto {
                model: m.model,
                count: m.count,
                cost_micros: m.cost_micros,
            })
            .collect(),
    }))
}

#[derive(Deserialize)]
pub struct SpendQuery {
    pub days: Option<i64>,
}

/// `GET /workspaces/:ws/summaries`
pub async fn list_summaries(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<SummaryDto>>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let query = SummaryQuery {
        include_archived: q.include_archived,
        text: non_blank(q.q),
        participant: non_blank(q.participant),
        tag: non_blank(q.tag),
        limit: q.limit.min(500),
        offset: q.offset,
    };
    let repo = state.repo.lock().expect("repo mutex");
    let records = repo.search_records(&workspace, &query)?;
    Ok(Json(records.into_iter().map(SummaryDto::from).collect()))
}

/// `GET /workspaces/:ws/summaries/:id`
pub async fn get_summary(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
) -> Result<Json<SummaryDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let record = repo
        .get_record(&workspace, &id)?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(SummaryDto::from(record)))
}

/// Re-read a record after a mutation, or 404 if it vanished.
fn reload(
    repo: &repository::SqliteRepository,
    workspace: &domain::WorkspaceId,
    id: &str,
    changed: bool,
) -> Result<Json<SummaryDto>, ApiError> {
    if !changed {
        return Err(ApiError::NotFound);
    }
    let record = repo.get_record(workspace, id)?.ok_or(ApiError::NotFound)?;
    Ok(Json(SummaryDto::from(record)))
}

macro_rules! flag_handler {
    ($name:ident, $method:ident, $value:expr) => {
        pub async fn $name(
            State(state): State<AppState>,
            user: AuthUser,
            Path((ws, id)): Path<(String, String)>,
        ) -> Result<Json<SummaryDto>, ApiError> {
            user.require_workspace(&ws)?;
            let workspace =
                domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
            let repo = state.repo.lock().expect("repo mutex");
            let changed = repo.$method(&workspace, &id, $value)?;
            reload(&repo, &workspace, &id, changed)
        }
    };
}

flag_handler!(pin, set_pinned, true);
flag_handler!(unpin, set_pinned, false);
flag_handler!(archive, set_archived, true);
flag_handler!(unarchive, set_archived, false);

#[derive(Deserialize)]
pub struct TagsRequest {
    pub tags: Vec<String>,
}

/// `PUT /workspaces/:ws/summaries/:id/tags`
pub async fn set_tags(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
    Json(body): Json<TagsRequest>,
) -> Result<Json<SummaryDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let changed = repo.set_tags(&workspace, &id, &body.tags)?;
    reload(&repo, &workspace, &id, changed)
}

/// `DELETE /workspaces/:ws/summaries/:id` — hard-delete one summary (DSH-013).
pub async fn delete_summary(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let deleted = {
        let repo = state.repo.lock().expect("repo mutex");
        repo.delete_record(&workspace, &id)?
    };
    if deleted {
        state.publish(crate::LiveEvent::summary_deleted(&workspace, &id));
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

/// Body for a bulk operation over a set of summary ids.
#[derive(Deserialize)]
pub struct BulkIdsRequest {
    pub ids: Vec<String>,
}

/// `DELETE /workspaces/:ws/summaries` — bulk hard-delete (DSH-013). Body:
/// `{ "ids": [...] }`. Returns how many were actually removed (ids not in this
/// workspace are silently skipped — tenant isolation, not an error).
pub async fn bulk_delete(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Json(body): Json<BulkIdsRequest>,
) -> Result<Json<BulkCountResponse>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let deleted: Vec<&String> = {
        let repo = state.repo.lock().expect("repo mutex");
        let mut deleted = Vec::new();
        for id in &body.ids {
            if repo.delete_record(&workspace, id)? {
                deleted.push(id);
            }
        }
        deleted
    };
    for id in &deleted {
        state.publish(crate::LiveEvent::summary_deleted(&workspace, id));
    }
    Ok(Json(BulkCountResponse {
        count: deleted.len(),
    }))
}

/// Body for bulk archive/unarchive.
#[derive(Deserialize)]
pub struct BulkArchiveRequest {
    pub ids: Vec<String>,
    pub archived: bool,
}

/// Count of rows affected by a bulk operation.
#[derive(Serialize)]
pub struct BulkCountResponse {
    pub count: usize,
}

/// `PATCH /workspaces/:ws/summaries` — bulk archive/unarchive (batches DSH-008).
/// Body: `{ "ids": [...], "archived": true }`. Returns how many changed.
pub async fn bulk_archive(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Json(body): Json<BulkArchiveRequest>,
) -> Result<Json<BulkCountResponse>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let mut count = 0;
    for id in &body.ids {
        if repo.set_archived(&workspace, id, body.archived)? {
            count += 1;
        }
    }
    Ok(Json(BulkCountResponse { count }))
}
