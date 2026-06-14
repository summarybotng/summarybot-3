//! WhatsApp export ingestion endpoint (PRD §2.3, WHA-001; ADR-121).
//!
//! Accepts a `.zip` (or a raw `_chat.txt`) upload as the request body, extracts
//! the transcript, parses it under a declared timezone + date order (the file
//! carries neither — WHA-020), then anonymizes/dedups/persists via the host
//! ingestor. Workspace-scoped + authenticated; re-uploading the same export is
//! idempotent at the message level (WHA-012).

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::Json;
use domain::{ChannelId, DateOrder, WorkspaceId};
use host::IngestContext;
use serde::{Deserialize, Serialize};

/// `?chat=<channel id>&tz=<IANA zone>&date_order=dmy|mdy`
#[derive(Deserialize)]
pub struct ImportQuery {
    /// The workspace channel id this chat is filed under (so schedules can scope
    /// to it). **Optional** — when omitted, the group name is auto-detected from
    /// the export's naming system lines (WHA-001; v2 ADR-081).
    #[serde(default)]
    pub chat: Option<String>,
    /// IANA timezone the export's local timestamps are in (default UTC).
    #[serde(default = "default_tz")]
    pub tz: String,
    /// Locale **hint** for ambiguous dates only — `dmy` (default) or `mdy`.
    /// The order is inferred from the data when possible; this is the fallback.
    #[serde(default)]
    pub date_order: Option<String>,
}

fn default_tz() -> String {
    "UTC".to_string()
}

/// Turn a detected group name into a stable, URL-safe channel id:
/// lowercase, non-alphanumerics collapsed to single hyphens, trimmed. Returns
/// `None` if nothing usable remains (so the caller asks for an explicit id).
fn slugify(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let slug = out.trim_matches('-').to_string();
    if slug.is_empty() {
        None
    } else {
        Some(slug)
    }
}

/// Outcome of an import.
#[derive(Serialize)]
pub struct ImportDto {
    pub chat_id: String,
    /// Whether `chat_id` was auto-detected from the export (no `chat` supplied).
    pub chat_auto_detected: bool,
    /// Detected export dialect (`Ios` / `Android`).
    pub format: String,
    pub messages: usize,
    pub stored: u64,
    pub duplicates: u64,
    pub new_participants: u64,
    pub date_start: Option<i64>,
    pub date_end: Option<i64>,
}

/// `POST /workspaces/:ws/whatsapp/imports` — body is the export (zip or text).
pub async fn import_whatsapp(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Query(q): Query<ImportQuery>,
    body: Bytes,
) -> Result<Json<ImportDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    if body.is_empty() {
        return Err(ApiError::bad_request("empty upload"));
    }
    // Resolve the chat id: an explicit `?chat=` wins; otherwise auto-detect the
    // group name from the export and slugify it (WHA-001; v2 ADR-081). A 1:1 chat
    // or a nameless export with no `chat` is a clear 400 asking for one.
    let explicit_chat = q.chat.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let chat_auto_detected = explicit_chat.is_none();
    let chat = match explicit_chat {
        Some(c) => ChannelId::parse(c).map_err(|e| ApiError::bad_request(e.to_string()))?,
        None => {
            let detected = host::detect_whatsapp_chat_name(&body)
                .map_err(|e| ApiError::bad_request(e.to_string()))?
                .and_then(|name| slugify(&name));
            let slug = detected.ok_or_else(|| {
                ApiError::bad_request(
                    "couldn't auto-detect a chat name from this export (e.g. a 1:1 chat); please provide a chat id",
                )
            })?;
            ChannelId::parse(slug).map_err(|e| ApiError::bad_request(e.to_string()))?
        }
    };
    let order = match q.date_order.as_deref() {
        Some("mdy") | Some("MDY") => DateOrder::MonthDayYear,
        _ => DateOrder::DayMonthYear,
    };

    // The HMAC key for phone anonymization (WHA-006) reuses the server's secret;
    // a dedicated key is a hardening refinement.
    let (summary, parsed) = {
        let repo = state.repo.lock().expect("repo mutex");
        let ctx = IngestContext {
            workspace_id: &workspace,
            chat_id: &chat,
            uploader: &user.0.sub,
        };
        host::ingest_whatsapp_zip(
            &*repo,
            &state.signing_key,
            &ctx,
            &body,
            &q.tz,
            order,
            crate::auth::now_secs(),
        )
        .map_err(|e| ApiError::bad_request(e.to_string()))?
    };

    Ok(Json(ImportDto {
        chat_id: chat.as_str().to_string(),
        chat_auto_detected,
        format: format!("{:?}", parsed.format),
        messages: parsed.messages.len(),
        stored: summary.stored,
        duplicates: summary.duplicates,
        new_participants: summary.new_participants,
        date_start: parsed.date_range.map(|(s, _)| s),
        date_end: parsed.date_range.map(|(_, e)| e),
    }))
}

/// One classified gap in a chat's coverage, with a copy-ready ask (WHA-017/019).
#[derive(Serialize)]
pub struct GapDto {
    pub start: i64,
    pub end: i64,
    /// `before_join` | `between_imports` | `after_last`.
    pub kind: String,
    /// Whether a member could plausibly export this range to fill it.
    pub can_fill: bool,
}

/// One member's contribution to a chat (WHA-018).
#[derive(Serialize)]
pub struct ContributionDto {
    pub uploader: String,
    pub import_count: i64,
    pub message_count: i64,
    pub earliest: i64,
    pub latest: i64,
}

/// A persisted scoped import invitation (WHA-019).
#[derive(Serialize)]
pub struct InvitationDto {
    pub id: String,
    pub chat_id: String,
    pub range_start: i64,
    pub range_end: i64,
    pub kind: String,
    pub note: String,
    pub status: String,
    pub created_by: String,
    pub created_at: i64,
    pub fulfilled_by: Option<String>,
    pub fulfilled_at: Option<i64>,
}

/// A chat's merged coverage picture (WHA-016), with contributors (WHA-018) and
/// any standing scoped invitations (WHA-019).
#[derive(Serialize)]
pub struct CoverageDto {
    pub chat_id: String,
    pub earliest: Option<i64>,
    pub latest: Option<i64>,
    /// Total seconds covered by the union of import spans.
    pub covered_secs: i64,
    pub gaps: Vec<GapDto>,
    pub contributors: Vec<ContributionDto>,
    pub invitations: Vec<InvitationDto>,
}

/// A chat in the workspace overview: its import stats plus coverage (WHA-017).
#[derive(Serialize)]
pub struct ChatCoverageDto {
    pub chat_id: String,
    pub import_count: i64,
    pub message_count: i64,
    pub coverage: CoverageDto,
}

fn gap_kind_tag(k: domain::GapKind) -> &'static str {
    match k {
        domain::GapKind::BeforeJoin => "before_join",
        domain::GapKind::BetweenImports => "between_imports",
        domain::GapKind::AfterLast => "after_last",
    }
}

fn contribution_dto(c: host::Contribution) -> ContributionDto {
    ContributionDto {
        uploader: c.uploader,
        import_count: c.import_count,
        message_count: c.message_count,
        earliest: c.earliest,
        latest: c.latest,
    }
}

fn invitation_dto(i: repository::ImportInvitation) -> InvitationDto {
    InvitationDto {
        id: i.id,
        chat_id: i.chat_id,
        range_start: i.range_start,
        range_end: i.range_end,
        kind: i.kind,
        note: i.note,
        status: i.status,
        created_by: i.created_by,
        created_at: i.created_at,
        fulfilled_by: i.fulfilled_by,
        fulfilled_at: i.fulfilled_at,
    }
}

fn coverage_dto(
    chat_id: String,
    report: domain::CoverageReport,
    contributors: Vec<host::Contribution>,
    invitations: Vec<repository::ImportInvitation>,
) -> CoverageDto {
    CoverageDto {
        chat_id,
        earliest: report.earliest,
        latest: report.latest,
        covered_secs: report.covered_secs,
        gaps: report
            .gaps
            .into_iter()
            .map(|g| GapDto {
                start: g.start,
                end: g.end,
                kind: gap_kind_tag(g.kind).to_string(),
                can_fill: g.can_fill,
            })
            .collect(),
        contributors: contributors.into_iter().map(contribution_dto).collect(),
        invitations: invitations.into_iter().map(invitation_dto).collect(),
    }
}

/// `GET /workspaces/:ws/whatsapp/chats` — per-chat coverage overview (WHA-017).
pub async fn list_chats(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
) -> Result<Json<Vec<ChatCoverageDto>>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let now = crate::auth::now_secs();
    let dtos = {
        let repo = state.repo.lock().expect("repo mutex");
        let chats =
            host::workspace_coverage(&*repo, &workspace, now).map_err(internal)?;
        chats
            .into_iter()
            .map(|c| {
                let channel = ChannelId::parse(&c.summary.chat_id)
                    .map_err(|e| ApiError::Internal(e.to_string()))?;
                let contributors =
                    host::contributors_for(&*repo, &workspace, &channel).map_err(internal)?;
                let invitations =
                    host::list_invitations(&*repo, &workspace, &channel).map_err(internal)?;
                Ok(ChatCoverageDto {
                    chat_id: c.summary.chat_id.clone(),
                    import_count: c.summary.import_count,
                    message_count: c.summary.message_count,
                    coverage: coverage_dto(c.summary.chat_id, c.report, contributors, invitations),
                })
            })
            .collect::<Result<Vec<_>, ApiError>>()?
    };
    Ok(Json(dtos))
}

fn internal(e: anyhow::Error) -> ApiError {
    ApiError::Internal(e.to_string())
}

/// `GET /workspaces/:ws/whatsapp/chats/:chat/coverage` — one chat's gaps (WHA-016).
pub async fn chat_coverage(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, chat)): Path<(String, String)>,
) -> Result<Json<CoverageDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let channel = ChannelId::parse(&chat).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let now = crate::auth::now_secs();
    let dto = {
        let repo = state.repo.lock().expect("repo mutex");
        let report = host::coverage_for(&*repo, &workspace, &channel, now).map_err(internal)?;
        let contributors = host::contributors_for(&*repo, &workspace, &channel).map_err(internal)?;
        let invitations = host::list_invitations(&*repo, &workspace, &channel).map_err(internal)?;
        coverage_dto(channel.as_str().to_string(), report, contributors, invitations)
    };
    Ok(Json(dto))
}

/// Outcome of a retrospective by-week run (WHA / ADR-088/089 Retrospective).
#[derive(Serialize)]
pub struct RetrospectiveDto {
    /// Weekly summaries produced (weeks with substantial content).
    pub produced: usize,
    /// Weeks in range that had no substantial messages (skipped, ADR-048).
    pub weeks_empty: usize,
    /// The produced summary ids (also visible on the Summaries tab).
    pub summary_ids: Vec<String>,
    /// True if the history exceeded the per-run week cap and only the most recent
    /// weeks were summarized (no silent truncation — surfaced to the caller).
    pub truncated: bool,
}

/// `POST /workspaces/:ws/whatsapp/chats/:chat/summarize-weeks` — retrospective
/// weekly summaries of an imported chat (ADR-088/089 "Past dates" applied per
/// week; ADR-101 weekly period; ADR-048 skip-empty). Walks the chat's covered
/// range in 7-day buckets, summarizing each week that has substantial messages
/// through the same per-tenant LLM + budget + delivery + knowledge pipeline as an
/// on-demand summary. Each weekly summary is stored (tagged `retrospective-weekly`)
/// dated to the week it covers, so they sort chronologically in the dashboard.
pub async fn summarize_weeks(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, chat)): Path<(String, String)>,
) -> Result<Json<RetrospectiveDto>, ApiError> {
    use domain::summarize::SummaryLength;
    use host::llm::{LlmProvider, RequestPriority, ResilientLlm};
    use host::{SummarizationService, SummarizeRequest};
    use repository::{SummaryRecord, WhatsAppRepository, WorkspaceSettingsRepository};

    /// Seconds in a week, and a per-run cap so a multi-year import can't run away.
    const WEEK: i64 = 604_800;
    const MAX_WEEKS: usize = 53;

    user.require_workspace(&ws)?;
    let workspace = WorkspaceId::parse(&ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let channel = ChannelId::parse(&chat).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let now = crate::auth::now_secs();

    // The chat's covered span defines the range to walk (the import records'
    // earliest start → latest end).
    let (start, end) = {
        let repo = state.repo.lock().expect("repo mutex");
        let report = host::coverage_for(&*repo, &workspace, &channel, now).map_err(internal)?;
        match (report.earliest, report.latest) {
            (Some(s), Some(e)) => (s, e),
            _ => return Err(ApiError::bad_request("no messages imported for this chat yet")),
        }
    };

    // Resolve the per-tenant LLM + workspace instructions once; the engine is
    // reused across weeks (the shared rate limiter still applies).
    let (resolution, instructions) = {
        let repo = state.repo.lock().expect("repo mutex");
        let r = crate::resolve_llm(&repo, &workspace, &state.model, state.master_key());
        let instructions = repo
            .get_settings(&workspace)
            .map_err(internal)?
            .summary_instructions;
        (r, instructions)
    };
    let ladder = state.ladder_for(&resolution.model);
    let engine = ResilientLlm::new(
        state.client_for_base(resolution.base_url.clone(), resolution.api_key.clone()),
        state.limiter.clone(),
    );

    // 7-day buckets across the range; cap to the most recent MAX_WEEKS.
    let mut buckets: Vec<(i64, i64)> = Vec::new();
    let mut s = start;
    while s <= end {
        buckets.push((s, (s + WEEK - 1).min(now)));
        s += WEEK;
    }
    let truncated = buckets.len() > MAX_WEEKS;
    if truncated {
        buckets = buckets.split_off(buckets.len() - MAX_WEEKS);
    }

    // Record this as a tracked job (ADR-040) so it shows in the Jobs view with
    // progress (weeks done / total) and cost, even though it runs synchronously.
    let total_weeks = buckets.len() as u32;
    let mut job = domain::Job::record(
        domain::JobId::parse(format!("job_retro_{}_{now}", channel.as_str()))
            .map_err(|e| ApiError::bad_request(e.to_string()))?,
        workspace.clone(),
        domain::JobType::Backfill,
        now,
    );
    let _ = job.start(now);
    job.set_progress(0, total_weeks, now);
    {
        let repo = state.repo.lock().expect("repo mutex");
        use repository::JobRepository;
        let _ = repo.create_job(&job);
    }

    let deliverers = state.deliverers();
    let mut summary_ids = Vec::new();
    let mut weeks_empty = 0usize;
    let mut total_cost = 0i64;
    let mut done = 0u32;
    for (wk_start, wk_end) in buckets {
        // Re-gate the budget each week so a long run stops cleanly when exhausted
        // (rather than failing the whole request or overspending silently).
        let charge = {
            let repo = state.repo.lock().expect("repo mutex");
            match crate::budget_gate(&repo, &resolution, now) {
                Ok(c) => c,
                Err(_) => break,
            }
        };
        let messages = {
            let repo = state.repo.lock().expect("repo mutex");
            repo.list_messages(&workspace, &channel, wk_start, wk_end)
                .map_err(internal)?
        };
        if !messages.iter().any(|m| m.is_substantial()) {
            weeks_empty += 1;
            continue;
        }
        let outcome = SummarizationService::new(&engine, &ladder)
            .summarize(&SummarizeRequest {
                messages: &messages,
                length: SummaryLength::Detailed,
                provider: LlmProvider::OpenRouter,
                priority: RequestPriority::Low,
                cap_micros: i64::MAX,
                instructions: instructions.as_deref(),
            })
            .map_err(|e| ApiError::bad_request(format!("{e:?}")))?;
        let record = SummaryRecord {
            id: format!("sum_retro_{}_{}", channel.as_str(), wk_start),
            channel_id: Some(channel.clone()),
            model: outcome.model,
            cost_micros: outcome.cost_micros,
            degraded: outcome.degraded,
            // Date the summary to the week it covers (clamped to now), so the
            // dashboard orders the retrospective digests chronologically.
            created_at: wk_end.min(now),
            pinned: false,
            archived: false,
            tags: vec!["retrospective-weekly".to_string()],
            coherence_score: Some(outcome.coherence.score),
            usage: outcome.usage,
            summary: outcome.summary,
        };
        {
            let repo = state.repo.lock().expect("repo mutex");
            let (destinations, caps) =
                host::load_workspace_delivery(&*repo, &workspace, state.master_key())
                    .map_err(internal)?;
            host::DeliveryService::new(&*repo)
                .with_deliverers(&deliverers)
                .deliver(&workspace, &record, &destinations, &caps)
                .map_err(internal)?;
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
        }
        state.publish(crate::LiveEvent::summary_created(&workspace, &record.id));
        total_cost += record.cost_micros;
        summary_ids.push(record.id);
        done += 1;
        // Persist progress as each week completes (visible if the job is polled).
        let repo = state.repo.lock().expect("repo mutex");
        use repository::JobRepository;
        job.set_progress(done, total_weeks, crate::auth::now_secs());
        let _ = repo.update_job(&job);
    }

    // Finalize the job (cost = total spent across the weeks produced).
    {
        let repo = state.repo.lock().expect("repo mutex");
        use repository::JobRepository;
        let fin = crate::auth::now_secs();
        let _ = job.complete(total_cost, fin);
        let _ = repo.update_job(&job);
    }

    Ok(Json(RetrospectiveDto {
        produced: summary_ids.len(),
        weeks_empty,
        summary_ids,
        truncated,
    }))
}

/// Body for opening a scoped import invitation (WHA-019).
#[derive(Deserialize)]
pub struct NewInvitationBody {
    pub range_start: i64,
    pub range_end: i64,
    /// `before_join` | `between_imports` | `after_last`.
    pub kind: String,
    /// Optional human-facing export instruction; a default is generated if absent.
    #[serde(default)]
    pub note: Option<String>,
}

/// `POST /workspaces/:ws/whatsapp/chats/:chat/invitations` — open a scoped ask
/// for a specific date range to be exported and uploaded (WHA-019).
pub async fn create_invitation(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, chat)): Path<(String, String)>,
    Json(body): Json<NewInvitationBody>,
) -> Result<Json<InvitationDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = WorkspaceId::parse(&ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let channel = ChannelId::parse(&chat).map_err(|e| ApiError::bad_request(e.to_string()))?;
    if body.range_end <= body.range_start {
        return Err(ApiError::bad_request("range_end must be after range_start"));
    }
    let note = body.note.unwrap_or_default();
    let now = crate::auth::now_secs();
    let dto = {
        let repo = state.repo.lock().expect("repo mutex");
        let id = host::open_invitation(
            &*repo,
            &workspace,
            &channel,
            body.range_start,
            body.range_end,
            &body.kind,
            &note,
            &user.0.sub,
            now,
        )
        .map_err(internal)?;
        let inv = repository::WhatsAppRepository::get_invitation(&*repo, &workspace, &id)
            .map_err(internal)?
            .ok_or_else(|| ApiError::Internal("invitation vanished after create".into()))?;
        invitation_dto(inv)
    };
    Ok(Json(dto))
}

/// `POST /workspaces/:ws/whatsapp/chats/:chat/invitations/:id/cancel` — withdraw
/// a standing ask (WHA-019).
pub async fn cancel_invitation(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, _chat, id)): Path<(String, String, String)>,
) -> Result<axum::http::StatusCode, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = WorkspaceId::parse(&ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let cancelled = {
        let repo = state.repo.lock().expect("repo mutex");
        host::cancel_invitation(&*repo, &workspace, &id).map_err(internal)?
    };
    if cancelled {
        Ok(axum::http::StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}
