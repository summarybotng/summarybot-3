//! RSS feeds of summaries (ADR-133 A4). Authenticated CRUD plus a public,
//! token-gated render endpoint that RSS readers can subscribe to. The token is
//! the capability; a non-public feed is served by token but marked `noindex`.

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize)]
pub struct FeedDto {
    pub id: String,
    pub channel_id: Option<String>,
    pub feed_type: String,
    pub is_public: bool,
    pub title: Option<String>,
    pub access_count: i64,
    pub created_at: i64,
    pub last_accessed: Option<i64>,
    /// The public URL path (`/feeds/<token>`) — the token is the capability.
    pub url: String,
}

#[derive(Deserialize)]
pub struct CreateFeedRequest {
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub is_public: bool,
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn to_dto(f: repository::Feed) -> FeedDto {
    FeedDto {
        url: format!("/feeds/{}", f.url_token),
        id: f.id,
        channel_id: f.channel_id,
        feed_type: f.feed_type,
        is_public: f.is_public,
        title: f.title,
        access_count: f.access_count,
        created_at: f.created_at,
        last_accessed: f.last_accessed,
    }
}

/// `GET /workspaces/:ws/feeds` — the workspace's feeds.
pub async fn list_feeds(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
) -> Result<Json<Vec<FeedDto>>, ApiError> {
    use repository::FeedRepository;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let feeds = repo
        .list_feeds(&workspace)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .into_iter()
        .map(to_dto)
        .collect();
    Ok(Json(feeds))
}

/// `POST /workspaces/:ws/feeds` — create an RSS feed (optionally channel-scoped).
pub async fn create_feed(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Json(body): Json<CreateFeedRequest>,
) -> Result<Json<FeedDto>, ApiError> {
    use repository::FeedRepository;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let token = host::random_url_token(18).map_err(ApiError::Internal)?;
    let now = now_secs();
    let feed = repository::Feed {
        id: format!("feed_{now}"),
        channel_id: body.channel_id.filter(|c| !c.trim().is_empty()),
        feed_type: "rss".to_string(),
        is_public: body.is_public,
        url_token: token,
        title: body.title.filter(|t| !t.trim().is_empty()),
        access_count: 0,
        created_at: now,
        last_accessed: None,
    };
    let repo = state.repo.lock().expect("repo mutex");
    repo.create_feed(&workspace, &feed)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(to_dto(feed)))
}

/// `DELETE /workspaces/:ws/feeds/:id`.
pub async fn delete_feed(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    use repository::FeedRepository;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let removed = repo
        .delete_feed(&workspace, &id)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(serde_json::json!({ "removed": removed })))
}

/// `GET /feeds/:token` — public RSS render of a feed's summaries. Unauthenticated;
/// the unguessable token is the capability. Bumps access_count.
pub async fn render_feed(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    use repository::{FeedRepository, StructuredSummaryRepository};
    let repo = state.repo.lock().expect("repo mutex");
    let Some((workspace, feed)) = repo.get_feed_by_token(&token).ok().flatten() else {
        return Err(StatusCode::NOT_FOUND);
    };
    let mut records = repo.list_records(&workspace, false, 50).unwrap_or_default();
    if let Some(ch) = &feed.channel_id {
        records.retain(|r| r.channel_id.as_ref().map(|c| c.as_str()) == Some(ch.as_str()));
    }
    let _ = repo.bump_feed_access(&token, now_secs());
    drop(repo);

    let title = feed.title.clone().unwrap_or_else(|| "SummaryBot feed".to_string());
    let mut items = String::new();
    for r in records.iter().take(30) {
        let first_line = r.summary.text.lines().next().unwrap_or("Summary").trim();
        let item_title = xml_escape(if first_line.is_empty() { "Summary" } else { first_line });
        let desc = xml_escape(&r.summary.text);
        items.push_str(&format!(
            "<item><title>{item_title}</title><guid isPermaLink=\"false\">{id}</guid>\
             <pubDate>{date}</pubDate><description>{desc}</description></item>",
            id = xml_escape(&r.id),
            date = rfc822(r.created_at),
        ));
    }
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <rss version=\"2.0\"><channel><title>{t}</title>\
         <description>Summaries from SummaryBot</description>{items}</channel></rss>",
        t = xml_escape(&title),
    );
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "application/rss+xml; charset=utf-8".parse().unwrap());
    if !feed.is_public {
        // Served by token, but keep private feeds out of search indexes.
        headers.insert("x-robots-tag", "noindex".parse().unwrap());
    }
    Ok((headers, body))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Minimal RFC-822 date (UTC/GMT) from a unix timestamp, for RSS `pubDate`.
fn rfc822(ts: i64) -> String {
    const DOW: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"]; // 1970-01-01 = Thu
    const MON: [&str; 12] =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let (h, mi, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    let dow = DOW[(days.rem_euclid(7)) as usize];
    // Civil date from days since epoch (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!(
        "{dow}, {d:02} {mon} {year} {h:02}:{mi:02}:{s:02} GMT",
        mon = MON[(m - 1) as usize],
    )
}
