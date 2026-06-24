//! A deterministic, no-network [`LlmClient`] for demos/tests/local runs.
//!
//! It produces a *structured* (JSON) extraction from the assembled prompt —
//! participants and key points pulled straight from the numbered messages — so
//! the full Phase-3 pipeline (validation, citation resolution, storage) runs
//! end-to-end with no API key. The real [`super::openrouter`] client swaps in
//! behind the same [`LlmClient`] trait when configured.

use super::engine::{LlmClient, LlmError, LlmRequest, LlmResponse};
use domain::summarize::FinishReason;

/// Deterministic stand-in for a real model.
#[derive(Debug, Clone, Copy, Default)]
pub struct DemoLlmClient;

impl LlmClient for DemoLlmClient {
    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let mut participants: Vec<String> = Vec::new();
        // Each key point is a grounded claim (ADR-004): it cites the message it
        // was lifted from by that message's [index].
        let mut key_points: Vec<serde_json::Value> = Vec::new();
        // The prompt is an instruction header + "[i] Author: content" lines
        // (see assemble_prompt). Only the numbered message lines are parsed.
        for line in request.prompt.lines() {
            if !line.starts_with('[') {
                continue;
            }
            // Recover the [index] so the key point can cite its source message.
            let index: usize = line
                .trim_start_matches('[')
                .split_once(']')
                .and_then(|(i, _)| i.trim().parse().ok())
                .unwrap_or(0);
            let after = line.split_once("] ").map(|(_, r)| r).unwrap_or(line);
            if let Some((author, content)) = after.split_once(": ") {
                let author = author.trim();
                if !author.is_empty() && !participants.iter().any(|p| p == author) {
                    participants.push(author.to_string());
                }
                let content = content.trim();
                if key_points.len() < 5 && !content.is_empty() {
                    key_points.push(serde_json::json!({
                        "text": content,
                        "citations": [index],
                        "confidence": 1.0,
                    }));
                }
            }
        }
        let text = if participants.is_empty() {
            "Summary.".to_string()
        } else {
            format!("Discussion among {}.", participants.join(", "))
        };
        let body = serde_json::json!({
            "text": text,
            "key_points": key_points,
            "action_items": [],
            "technical_terms": [],
            "participants": participants,
            "citations": [],
        });
        Ok(LlmResponse {
            model: "demo".to_string(),
            text: body.to_string(),
            finish_reason: FinishReason::Stop,
            usage: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{LlmProvider, RequestPriority};

    fn req(prompt: &str) -> LlmRequest {
        LlmRequest {
            provider: LlmProvider::OpenRouter,
            priority: RequestPriority::Manual,
            model: "demo".into(),
            prompt: prompt.into(),
        }
    }

    #[test]
    fn extracts_participants_and_points_from_the_prompt() {
        let resp = DemoLlmClient
            .complete(&req(
                "[0] Alice: ship it friday\n[1] Bob: ill do the changelog",
            ))
            .unwrap();
        assert_eq!(resp.model, "demo");
        let v: serde_json::Value = serde_json::from_str(&resp.text).unwrap();
        assert_eq!(v["participants"], serde_json::json!(["Alice", "Bob"]));
        // Per-claim grounded key point (ADR-004): text + the cited message index.
        assert_eq!(v["key_points"][0]["text"], "ship it friday");
        assert_eq!(v["key_points"][0]["citations"], serde_json::json!([0]));
        assert_eq!(resp.finish_reason, FinishReason::Stop);
    }

    #[test]
    fn empty_prompt_still_valid_json() {
        let resp = DemoLlmClient.complete(&req("")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&resp.text).unwrap();
        assert_eq!(v["text"], "Summary.");
        assert!(v["participants"].as_array().unwrap().is_empty());
    }
}
