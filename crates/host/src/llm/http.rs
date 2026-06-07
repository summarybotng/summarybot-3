//! OpenAI-compatible HTTP LLM client (ADR-125) — the network last mile.
//!
//! A blocking [`LlmClient`] that POSTs to any OpenAI-style `chat/completions`
//! endpoint: OpenRouter, OpenAI, or a local Ollama/LM Studio server (the
//! operator's Mac mini). It is parameterized by a `base_url` and an *optional*
//! bearer key — a local endpoint needs no key. It maps HTTP status →
//! [`FailureClass`] (LEG-002) and the `Retry-After` / `x-ratelimit-*` headers
//! (LEG-003) → a retry hint; resilience (rate limiting, retries, circuit) is
//! [`ResilientLlm`]'s job, not this client's.
//!
//! Feature-gated (`http-llm`) so the default build stays network-free.

use super::engine::{LlmClient, LlmError, LlmRequest, LlmResponse};
use super::{classify_http_status, FailureClass, LlmProvider};
use domain::summarize::FinishReason;
use domain::Secret;

const OPENROUTER_BASE: &str = "https://openrouter.ai/api/v1";

/// An OpenAI-compatible chat-completions client. The key is held as a
/// [`Secret`] (when present) so it can't leak into logs.
pub struct HttpLlmClient {
    /// API root, e.g. `https://openrouter.ai/api/v1` or `http://mac-mini:11434/v1`.
    base_url: String,
    /// Bearer key; `None` for keyless local endpoints (Ollama/LM Studio).
    api_key: Option<Secret<String>>,
    /// Optional per-call timeout (seconds); `None` uses the agent default.
    timeout_secs: Option<u64>,
    /// Ollama-only `options.num_ctx` (context window) — best-effort. Set this for
    /// a local Ollama whose default (~4k) truncates a large prompt. NOTE: Ollama's
    /// OpenAI-compatible `/v1/chat/completions` endpoint *ignores* this field
    /// (only its native `/api/*` reads it), so the durable defense against
    /// truncation is putting the JSON instruction last in the prompt (see
    /// `summarize::assemble_prompt`). Kept for the providers/endpoints that honor
    /// it; omitted from the request when `None` so hosted providers are unaffected.
    num_ctx: Option<u32>,
}

impl HttpLlmClient {
    /// Build a client against an arbitrary OpenAI-compatible `base_url`, with an
    /// optional bearer key.
    pub fn new(base_url: impl Into<String>, api_key: Option<Secret<String>>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            timeout_secs: Some(60),
            num_ctx: None,
        }
    }

    /// Set the Ollama context window (`options.num_ctx`) — best-effort; honored
    /// by Ollama's native `/api/*` but ignored by its OpenAI-compatible `/v1`.
    /// No effect on non-Ollama providers beyond an extra field.
    pub fn with_num_ctx(mut self, num_ctx: u32) -> Self {
        self.num_ctx = Some(num_ctx);
        self
    }

    /// Convenience: the hosted OpenRouter endpoint with a key.
    pub fn openrouter(api_key: Secret<String>) -> Self {
        Self::new(OPENROUTER_BASE, Some(api_key))
    }

    pub fn with_timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs);
        self
    }

    /// `{base_url}/chat/completions`, tolerating a trailing slash on the base.
    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    fn agent(&self) -> ureq::Agent {
        let mut builder = ureq::AgentBuilder::new();
        if let Some(secs) = self.timeout_secs {
            builder = builder.timeout(std::time::Duration::from_secs(secs));
        }
        builder.build()
    }
}

impl LlmClient for HttpLlmClient {
    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let mut body = serde_json::json!({
            "model": request.model,
            "messages": [{ "role": "user", "content": request.prompt }],
        });
        if let Some(n) = self.num_ctx {
            // Ollama reads options.num_ctx; other providers ignore/omit it.
            body["options"] = serde_json::json!({ "num_ctx": n });
        }
        let mut req = self
            .agent()
            .post(&self.endpoint())
            .set("Content-Type", "application/json");
        // Local endpoints (Ollama/LM Studio) accept no Authorization header.
        if let Some(key) = &self.api_key {
            req = req.set("Authorization", &format!("Bearer {}", key.expose_secret()));
        }

        match req.send_json(body) {
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
    // Prefer an explicit Retry-After (seconds); fall back to the rate-limit
    // reset header (LEG-003). Local endpoints send neither — that's fine.
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
        detail: format!("llm http {code}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_joins_base_and_tolerates_trailing_slash() {
        let a = HttpLlmClient::new("http://mac-mini:11434/v1", None);
        assert_eq!(a.endpoint(), "http://mac-mini:11434/v1/chat/completions");
        let b = HttpLlmClient::new("http://mac-mini:11434/v1/", None);
        assert_eq!(b.endpoint(), "http://mac-mini:11434/v1/chat/completions");
    }

    #[test]
    fn openrouter_ctor_targets_the_hosted_endpoint() {
        let c = HttpLlmClient::openrouter(Secret::new("k".to_string()));
        assert_eq!(
            c.endpoint(),
            "https://openrouter.ai/api/v1/chat/completions"
        );
    }
}
