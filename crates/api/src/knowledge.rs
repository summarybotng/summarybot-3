//! Knowledge subsystem endpoints (PRD §8.1; ADR-127) — semantic search over the
//! units extracted from a workspace's summaries, plus the ingest helper the
//! summary paths call after a summary is stored.

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::extract::{Path, Query, State};
use axum::Json;
use domain::summarize::ExtractedSummary;
use host::KnowledgeService;
use repository::SqliteRepository;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct SearchQuery {
    /// The natural-language query.
    #[serde(default)]
    pub q: String,
    /// Max results (default 10, capped at 50).
    pub k: Option<usize>,
}

#[derive(Serialize)]
pub struct HitDto {
    pub id: String,
    pub summary_id: String,
    pub kind: String,
    pub text: String,
    pub source_ids: Vec<String>,
    pub score: f32,
}

/// `GET /workspaces/:ws/wiki/search?q=...&k=...` — semantic search (KNO-005).
pub async fn search(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<HitDto>>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let q = query.q.trim();
    if q.is_empty() {
        return Ok(Json(vec![]));
    }
    let k = query.k.unwrap_or(10).min(50);
    let repo = state.repo.lock().expect("repo mutex");
    let hits = KnowledgeService::new(&*repo, &*state.embedder)
        .search(&workspace, q, k)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(
        hits.into_iter()
            .map(|h| HitDto {
                id: h.id,
                summary_id: h.summary_id,
                kind: h.kind,
                text: h.text,
                source_ids: h.source_ids,
                score: h.score,
            })
            .collect(),
    ))
}

/// JSON shape of a synthesized wiki page (WIK-001..003).
#[derive(Serialize)]
pub struct WikiPageDto {
    pub slug: String,
    pub title: String,
    pub content_md: String,
    /// How many knowledge units fed the most recent synthesis.
    pub unit_count: i64,
    pub updated_at: i64,
}

impl From<repository::WikiPage> for WikiPageDto {
    fn from(p: repository::WikiPage) -> Self {
        WikiPageDto {
            slug: p.slug,
            title: p.title,
            content_md: p.content_md,
            unit_count: p.unit_count,
            updated_at: p.updated_at,
        }
    }
}

/// `GET /workspaces/:ws/wiki/pages` — list the workspace's synthesized pages
/// (WIK-003). v1 maintains a single `knowledge-base` page; the list is empty
/// until the first synthesis.
pub async fn list_pages(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
) -> Result<Json<Vec<WikiPageDto>>, ApiError> {
    use repository::WikiRepository;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let pages = repo
        .list_pages(&workspace)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(pages.into_iter().map(WikiPageDto::from).collect()))
}

/// `POST /workspaces/:ws/wiki/synthesize` — (re)generate the `knowledge-base`
/// page from the workspace's knowledge units (WIK-001), over the per-tenant LLM
/// (ADR-125) and gated/charged against the tenant budget like an on-demand
/// summary. Returns the synthesized page.
pub async fn synthesize(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
) -> Result<Json<WikiPageDto>, ApiError> {
    use host::llm::{LlmProvider, RequestPriority, ResilientLlm};
    use host::{WikiError, WikiService};

    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let now = crate::auth::now_secs();

    // Resolve per-tenant LLM + gate on budget under one guard (as create_summary).
    let (resolution, charge) = {
        let repo = state.repo.lock().expect("repo mutex");
        let r = crate::resolve_llm(&repo, &workspace, &state.model, state.master_key());
        let charge = crate::budget_gate(&repo, &r, now)?;
        (r, charge)
    };
    let ladder = state.ladder_for(&resolution.model);
    let engine = ResilientLlm::new(
        state.client_for_base(resolution.base_url, resolution.api_key),
        state.limiter.clone(),
    );

    let page = {
        let repo = state.repo.lock().expect("repo mutex");
        let outcome = WikiService::new(&*repo, &engine, &ladder)
            .synthesize(
                &workspace,
                LlmProvider::OpenRouter,
                RequestPriority::Manual,
                i64::MAX,
                now,
            )
            .map_err(|e| match e {
                WikiError::NoUnits => ApiError::bad_request(e.to_string()),
                _ => ApiError::Internal(e.to_string()),
            })?;
        // Draw down the tenant's budget by what synthesis cost (ADR-125).
        if let Some((tenant, window)) = &charge {
            crate::budget_charge(&repo, tenant, *window, outcome.cost_micros)?;
        }
        outcome.page
    };
    Ok(Json(WikiPageDto::from(page)))
}

/// `?stale_days=N` — units older than this are flagged stale (default 90).
#[derive(Deserialize)]
pub struct CurateQuery {
    pub stale_days: Option<i64>,
}

/// One near-identical group from the curator.
#[derive(Serialize)]
pub struct DuplicateClusterDto {
    pub canonical_id: String,
    pub kind: String,
    pub text: String,
    pub duplicate_ids: Vec<String>,
}

/// A unit flagged old enough to review.
#[derive(Serialize)]
pub struct StaleUnitDto {
    pub id: String,
    pub kind: String,
    pub text: String,
    pub age_secs: i64,
}

/// The curator's read-only knowledge-base health report (CUR-*).
#[derive(Serialize)]
pub struct CurationReportDto {
    pub total_units: usize,
    pub embedded_units: usize,
    pub redundant_count: usize,
    pub duplicate_clusters: Vec<DuplicateClusterDto>,
    pub stale: Vec<StaleUnitDto>,
}

/// `POST /workspaces/:ws/wiki/curate?stale_days=N` — the AI wiki curator's
/// advisory health report: duplicate clusters + stale units (CUR-*, ADR-127).
/// Read-only (never mutates), so it's inherently reversible; the run is
/// audit-logged. No LLM call — deterministic over the stored embeddings.
pub async fn curate(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Query(q): Query<CurateQuery>,
) -> Result<Json<CurationReportDto>, ApiError> {
    use host::CuratorService;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let now = crate::auth::now_secs();
    let stale_after = q.stale_days.unwrap_or(90).max(0) * 86_400;

    let repo = state.repo.lock().expect("repo mutex");
    let report = CuratorService::new(&*repo)
        .curate(&workspace, now, stale_after)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    // Audit the curation run (best-effort) so review activity is traceable.
    use repository::IdentityRepository;
    let _ = repo.append_audit(&repository::AuditEntry {
        ts: now,
        actor: Some(user.0.sub.clone()),
        action: "knowledge.curated".to_string(),
        detail: format!(
            "{} units, {} redundant, {} stale",
            report.total_units,
            report.redundant_count(),
            report.stale.len()
        ),
    });
    Ok(Json(CurationReportDto {
        total_units: report.total_units,
        embedded_units: report.embedded_units,
        redundant_count: report.redundant_count(),
        duplicate_clusters: report
            .duplicate_clusters
            .into_iter()
            .map(|c| DuplicateClusterDto {
                canonical_id: c.canonical_id,
                kind: c.kind,
                text: c.text,
                duplicate_ids: c.duplicate_ids,
            })
            .collect(),
        stale: report
            .stale
            .into_iter()
            .map(|u| StaleUnitDto {
                id: u.id,
                kind: u.kind,
                text: u.text,
                age_secs: u.age_secs,
            })
            .collect(),
    }))
}

/// Ingest a produced summary's knowledge units (KNO-001..003). Best-effort: a
/// failure (e.g. the embedder is down) is logged, never propagated — knowledge
/// ingestion must not fail the summary that was already stored/delivered.
/// Call with the repo lock held.
pub(crate) fn ingest_summary(
    state: &AppState,
    repo: &SqliteRepository,
    workspace: &domain::WorkspaceId,
    summary: &ExtractedSummary,
    summary_id: &str,
    now: i64,
) {
    if let Err(e) =
        KnowledgeService::new(repo, &*state.embedder).ingest(workspace, summary, summary_id, now)
    {
        eprintln!("knowledge ingest failed for {summary_id}: {e}");
    }
}
