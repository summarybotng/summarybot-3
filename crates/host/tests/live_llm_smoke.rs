//! Opt-in live-backend smoke test (ADR-134 §4 — the net for the "stub blind
//! spot"). It hits the *real* configured LLM once and asserts the things stub
//! tests can't see: a real model id is accepted (the `"demo"`-default bug, #1),
//! and the provider's `usage` block is parsed into non-zero token counts (#2).
//!
//! Hermetic by default: with no LLM env configured it SKIPS (passes trivially),
//! so CI and `cargo test` stay network-free. To actually exercise a backend:
//!
//!   OPENROUTER_API_KEY=sk-or-... LLM_MODEL=anthropic/claude-haiku-4.5 \
//!     cargo test -p host --features http-llm --test live_llm_smoke -- --nocapture
//!
//! or against a local OpenAI-compatible endpoint:
//!
//!   LLM_BASE_URL=http://mac-mini:11434/v1 LLM_MODEL=qwen2.5:14b \
//!     cargo test -p host --features http-llm --test live_llm_smoke -- --nocapture

#![cfg(feature = "http-llm")]

use domain::Secret;
use host::llm::{HttpLlmClient, LlmClient, LlmProvider, LlmRequest, RequestPriority};

fn nonempty(var: &str) -> Option<String> {
    std::env::var(var).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

#[test]
fn live_backend_returns_content_and_usage() {
    // Resolve the backend the same way main.rs does: an explicit LLM_BASE_URL
    // (+ optional key) wins; otherwise OPENROUTER_API_KEY; otherwise skip.
    let base = nonempty("LLM_BASE_URL");
    let openrouter_key = nonempty("OPENROUTER_API_KEY");
    let model = nonempty("LLM_MODEL").unwrap_or_else(|| "anthropic/claude-haiku-4.5".to_string());

    let (client, is_openrouter) = if let Some(base) = base {
        (HttpLlmClient::new(base, nonempty("LLM_API_KEY").map(Secret::new)), false)
    } else if let Some(key) = openrouter_key {
        (HttpLlmClient::openrouter(Secret::new(key)), true)
    } else {
        eprintln!(
            "SKIP live_backend_returns_content_and_usage: set OPENROUTER_API_KEY or LLM_BASE_URL \
             (+ optional LLM_MODEL) to run it"
        );
        return;
    };

    let req = LlmRequest {
        provider: LlmProvider::OpenRouter,
        priority: RequestPriority::Manual,
        model: model.clone(),
        prompt: "Reply with exactly one word: pong.".to_string(),
    };

    let resp = client
        .complete(&req)
        .unwrap_or_else(|e| panic!("live LLM call failed (model {model}): {} — {}", e.class.as_str(), e.detail));

    assert!(!resp.text.trim().is_empty(), "expected non-empty content from the model");

    match resp.usage {
        Some(u) => {
            eprintln!("live LLM ok: model={model} {} in / {} out tokens", u.prompt_tokens, u.completion_tokens);
            assert!(
                u.prompt_tokens > 0 && u.completion_tokens > 0,
                "provider reported a usage block but with zero tokens: {u:?}",
            );
        }
        None if is_openrouter => panic!(
            "OpenRouter returned no usage block — token/cost accounting (ADR-134 #2) would silently \
             read zero. Did the usage-parsing regress?"
        ),
        None => eprintln!(
            "WARN: local endpoint reported no usage block; counts will fall back to estimates (acceptable)"
        ),
    }
}
