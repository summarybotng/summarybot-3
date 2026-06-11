//! End-to-end spine: WhatsApp ingest → stored messages → summarize → deliver.
//!
//! Proves Phases 2→3→4 compose against real stored data with a fake LLM (no
//! network): a WhatsApp export is parsed/anonymized/fingerprinted and stored;
//! the summarizer reads those messages back, produces a validated structured
//! summary with a grounded citation resolved to a real stored message id; and
//! delivery persists it to the always-on dashboard store.

use domain::summarize::{Model, ModelLadder, ModelPrice, SummaryLength};
use domain::{
    parse_export, ChannelId, DateOrder, DeliveryCapabilities, Secret, UserId, WorkspaceId,
};
use host::llm::{
    GlobalRateLimiter, LlmClient, LlmError, LlmProvider, LlmRequest, LlmResponse, RateLimitConfig,
    RequestPriority, ResilientLlm,
};
use host::{
    DeliveryService, IngestContext, SummarizationService, SummarizeRequest, WhatsAppIngestor,
};
use repository::{StructuredSummaryRepository, SummaryRecord, WhatsAppRepository};

/// Returns a fixed structured summary citing the first message (index 0).
struct FakeLlm;
impl LlmClient for FakeLlm {
    fn complete(&self, _req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            model: "fake".into(),
            text: r#"{
                "text": "The team agreed to ship the release on Friday.",
                "key_points": ["Ship the release on Friday"],
                "action_items": [{"text": "write the changelog", "assignee": "Bob"}],
                "technical_terms": [],
                "participants": ["Alice", "Bob"],
                "citations": [{"message_index": 0, "quote": "ship the release"}]
            }"#
            .into(),
            finish_reason: domain::summarize::FinishReason::Stop,
        })
    }
}

fn ladder() -> ModelLadder {
    ModelLadder::new(vec![Model {
        name: "fake".into(),
        price: ModelPrice {
            input_micros_per_ktoken: 100,
            output_micros_per_ktoken: 100,
        },
        context_tokens: 200_000,
    }])
}

#[test]
fn ingest_then_summarize_then_deliver() {
    let repo = repository::SqliteRepository::in_memory().unwrap();
    let ws = WorkspaceId::parse("ws-1").unwrap();
    let chat = ChannelId::parse("chat-1").unwrap();
    let uploader = UserId::parse("uploader-1").unwrap();
    let anon_key = Secret::new(b"anon".to_vec());

    // --- Phase 2: ingest a WhatsApp export → stored, fingerprinted messages.
    let export = "[01/01/2026, 09:00:00] Alice: lets ship the release on friday\n\
                  [01/01/2026, 09:01:00] Bob: ill write the changelog";
    let parsed = parse_export(export, chrono_tz::Europe::London, DateOrder::DayMonthYear);
    let ctx = IngestContext {
        workspace_id: &ws,
        chat_id: &chat,
        uploader: &uploader,
    };
    let summary = WhatsAppIngestor::new(&repo, &anon_key)
        .ingest(&ctx, &parsed.messages)
        .unwrap();
    assert_eq!(summary.stored, 2);

    // Read them back as the summarizer would (the new list query).
    let messages = repo.list_messages(&ws, &chat, 0, i64::MAX).unwrap();
    assert_eq!(messages.len(), 2);
    let first_id = messages[0].id.clone();

    // --- Phase 3: summarize the stored messages via the resilient pipeline.
    let limiter = std::sync::Arc::new(GlobalRateLimiter::new(RateLimitConfig::default()));
    let engine = ResilientLlm::new(FakeLlm, limiter);
    let l = ladder();
    let svc = SummarizationService::new(&engine, &l);
    let outcome = svc
        .summarize(&SummarizeRequest {
            messages: &messages,
            length: SummaryLength::Detailed,
            provider: LlmProvider::OpenRouter,
            priority: RequestPriority::Manual,
            cap_micros: i64::MAX,
            instructions: None,
        })
        .expect("summary produced");

    assert_eq!(
        outcome.summary.key_points,
        vec!["Ship the release on Friday".to_string()]
    );
    // The grounded citation resolved to a real stored message id (ADR-004).
    assert_eq!(outcome.summary.citations.len(), 1);
    assert_eq!(outcome.summary.citations[0].message_id, first_id);

    // --- Phase 4: deliver → always-on dashboard store.
    let record = SummaryRecord {
        id: "sum_e2e".into(),
        channel_id: Some(chat.clone()),
        model: outcome.model,
        cost_micros: outcome.cost_micros,
        degraded: outcome.degraded,
        created_at: 1_700_000_000,
        pinned: false,
        archived: false,
        tags: vec![],
        summary: outcome.summary.clone(),
    };
    let report = DeliveryService::new(&repo)
        .deliver(&ws, &record, &[], &DeliveryCapabilities::default())
        .unwrap();
    assert_eq!(report.delivered_count(), 1); // dashboard store

    // The full structured summary round-trips out of the dashboard store, with
    // the citation still pointing at the ingested message.
    let stored = repo.get_record(&ws, "sum_e2e").unwrap().unwrap();
    assert_eq!(stored.summary, outcome.summary);
    assert_eq!(stored.summary.citations[0].message_id, first_id);
}
