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
        use repository::WorkspaceSettingsRepository;
        let repo = state.repo.lock().expect("repo mutex");
        let r = crate::resolve_llm(&repo, &workspace, &state.model, state.master_key());
        let charge = crate::budget_gate(&repo, &r, now)?;
        let instructions = repo
            .get_settings(&workspace)
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .summary_instructions;
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
    let mut job = domain::Job::record(
        domain::JobId::parse(format!("job_sum_{}", unique_suffix()))
            .map_err(|e| ApiError::bad_request(e.to_string()))?,
        workspace.clone(),
        domain::JobType::Summarization,
        now,
    );
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
        let _ = job.complete(record.cost_micros, crate::auth::now_secs());
        let _ = repo.update_job(&job);
    }
    state.publish(crate::LiveEvent::summary_created(&workspace, &record.id));
    Ok(Json(SummaryDto::from(record)))
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
