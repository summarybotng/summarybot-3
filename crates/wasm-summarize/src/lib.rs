//! Sandboxed pure-compute WASM component (PRD §12.0). It exports the typed
//! `summarize` function defined in `wit/world.wit` and delegates to the shared
//! `domain` crate, so the native host and the WASM guest run identical logic.

// Generate guest bindings from the WIT world. `path` is relative to this crate.
wit_bindgen::generate!({
    world: "compute",
    path: "../../wit",
});

use exports::summarybot::compute::summarize::{Guest, SummaryRequest, SummaryResult};

struct Component;

impl Guest for Component {
    fn summarize(req: SummaryRequest) -> SummaryResult {
        // The host has already validated the workspace id; inside the sandbox
        // we re-parse defensively and fall back to a sentinel that the host
        // never persists without its own validated id.
        let workspace_id = domain::WorkspaceId::parse(req.workspace_id)
            .unwrap_or_else(|_| domain::WorkspaceId::parse("invalid").unwrap());
        let input = domain::SummaryInput {
            workspace_id,
            messages: req.messages,
        };
        let s = domain::summarize(&input);
        SummaryResult {
            text: s.text,
            message_count: s.message_count,
            word_count: s.word_count,
        }
    }
}

export!(Component);
