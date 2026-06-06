//! Concrete OpenRouter LLM client (LEG-003) — the network last mile.
//!
//! A blocking [`LlmClient`] that POSTs to OpenRouter's chat-completions endpoint
//! and maps the result onto our taxonomy: HTTP status → [`FailureClass`]
//! (LEG-002), the `Retry-After` / `x-ratelimit-*` headers (LEG-003,
//! [`LlmProvider::parse_rate_limit`]) → the retry hint. The resilience around it
//! — rate limiting, retries, circuit — is [`ResilientLlm`]'s job, not this
//! client's.
//!
//! Feature-gated (`openrouter`) so the default build stays network-free; this is
//! exercised by integration/manual runs, not the unit suite.

use super::engine::{LlmClient, LlmError, LlmRequest, LlmResponse};
use super::{classify_http_status, FailureClass, LlmProvider};
use domain::summarize::FinishReason;
use domain::Secret;

const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

/// OpenRouter-backed client. Holds the API key as a [`Secret`] so it can't leak.
pub struct OpenRouterClient {
    api_key: Secret<String>,
    /// Optional per-call timeout (seconds); `None` uses the agent default.
    timeout_secs: Option<u64>,
}

impl OpenRouterClient {
    pub fn new(api_key: Secret<String>) -> Self {
        Self {
            api_key,
            timeout_secs: Some(60),
        }
    }

    pub fn with_timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs);
        self
    }

    fn agent(&self) -> ureq::Agent {
        let mut builder = ureq::AgentBuilder::new();
        if let Some(secs) = self.timeout_secs {
            builder = builder.timeout(std::time::Duration::from_secs(secs));
        }
        builder.build()
    }
}

impl LlmClient for OpenRouterClient {
    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let body = serde_json::json!({
            "model": request.model,
            "messages": [{ "role": "user", "content": request.prompt }],
        });
        let result = self
            .agent()
            .post(ENDPOINT)
            .set(
                "Authorization",
                &format!("Bearer {}", self.api_key.expose_secret()),
            )
            .set("Content-Type", "application/json")
            .send_json(body);

        match result {
            Ok(response) => parse_success(request, response),
            // Non-2xx with a response body: classify by status + headers.
            Err(ureq::Error::Status(code, response)) => Err(status_error(code, &response)),
            // Transport/connection failure: transient.
            Err(ureq::Error::Transport(t)) => Err(LlmError {
                class: FailureClass::ServiceUnavailable,
                retry_after_secs: None,
                detail: format!("transport error: {t}"),
            }),
        }
    }
}

fn parse_success(request: &LlmRequest, response: ureq::Response) -> Result<LlmResponse, LlmError> {
    let value: serde_json::Value = response.into_json().map_err(|e| LlmError {
        class: FailureClass::Unknown,
        retry_after_secs: None,
        detail: format!("malformed success body: {e}"),
    })?;
    let text = value
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LlmError {
            class: FailureClass::Unknown,
            retry_after_secs: None,
            detail: "response missing choices[0].message.content".to_string(),
        })?;
    let finish_reason = match value
        .pointer("/choices/0/finish_reason")
        .and_then(|v| v.as_str())
    {
        Some("stop") => FinishReason::Stop,
        Some("length") => FinishReason::Length,
        Some("content_filter") => FinishReason::ContentFilter,
        _ => FinishReason::Other,
    };
    Ok(LlmResponse {
        model: request.model.clone(),
        text: text.to_string(),
        finish_reason,
    })
}

fn status_error(code: u16, response: &ureq::Response) -> LlmError {
    let class = classify_http_status(code);
    // Prefer an explicit Retry-After (seconds); fall back to the provider's
    // rate-limit reset header parsed via LEG-003.
    let retry_after_secs = response
        .header("retry-after")
        .and_then(|v| v.trim().parse::<i64>().ok())
        .or_else(|| {
            LlmProvider::OpenRouter
                .parse_rate_limit(|h| response.header(h))
                .reset_at_unix
        });
    LlmError {
        class,
        retry_after_secs,
        detail: format!("openrouter http {code}"),
    }
}
