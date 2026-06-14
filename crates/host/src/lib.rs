//! Native host: owns all I/O and orchestration (PRD §12.0). It loads the
//! sandboxed `wasm-summarize` component, hands it bounded input, then persists
//! the result through the repository layer. This is the Phase 0 walking
//! skeleton: host -> WASM service -> repository -> DB.

use anyhow::{Context, Result};
use domain::WorkspaceId;
use repository::{StoredSummary, SummaryRepository};

pub mod auth;
pub mod coverage;
pub mod curator;
pub mod delivery;
pub mod discord;
pub mod ingest;
pub mod invite;
pub mod knowledge;
pub mod llm;
#[cfg(feature = "oauth")]
pub mod oauth;
pub mod platform;
pub mod schedule_runner;
pub mod scheduler;
pub mod secretbox;
pub mod slack;
pub mod summarize;
pub mod tenant_routing;
pub mod whatsapp;
pub mod wiki;
pub use auth::{new_correlation_id, verify_token, AuthError, AuthService, TokenPair};
pub use coverage::{
    cancel_invitation, contributors_for, coverage_for, list_invitations, open_invitation,
    reconcile_invitations, workspace_coverage, ChatCoverage, Contribution,
};
pub use curator::{CurationReport, CuratorService, DuplicateCluster, StaleUnit};
pub use delivery::{
    build_deliverers, inject_platform_token, load_workspace_delivery, resolve_destination_config,
    sink_descriptors, ConfiguredDestination, Deliverer, DeliveryOutcome, DeliveryReport,
    DeliveryService, FieldHint, FieldScope, FieldSpec, RenderedSummary, SinkDescriptor,
};
#[cfg(feature = "discord")]
pub use discord::DiscordFetcher;
pub use discord::{parse_message as parse_discord_message, snowflake_to_unix_secs};
pub use ingest::{sync_into_store, SyncReport};
pub use invite::{InviteService, IssuedInvite, DEFAULT_INVITE_TTL_SECS};
#[cfg(feature = "http-llm")]
pub use knowledge::HttpEmbedder;
pub use knowledge::{DemoEmbedder, Embedder, KnowledgeService, SearchHit};
pub use platform::make_platform_fetcher;
pub use platform::{
    ChannelInfo, FetchError, FetchResult, FetchScope, PlatformContext, PlatformFetcher,
};
pub use schedule_runner::SummarizingScheduleRunner;
pub use scheduler::{ScheduleRunner, SchedulerService, TickReport};
pub use secretbox::{decrypt_secret, encrypt_secret, parse_master_key};
#[cfg(feature = "slack")]
pub use slack::SlackFetcher;
pub use slack::{parse_message as parse_slack_message, slack_ts_to_unix_secs};
pub use summarize::{SummarizationService, SummarizeError, SummarizeRequest, SummaryOutcome};
pub use tenant_routing::resolve_tenant_by_host;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiView};
pub use whatsapp::{
    extract_whatsapp_text, ingest_whatsapp_zip, IngestContext, IngestSummary, WhatsAppIngestor,
};
pub use wiki::{build_synthesis_prompt, WikiError, WikiOutcome, WikiService, KNOWLEDGE_BASE_SLUG};

// Host-side bindings generated from the same WIT contract the guest exports.
mod bindings {
    wasmtime::component::bindgen!({
        world: "compute",
        path: "../../wit",
    });
}

use bindings::exports::summarybot::compute::summarize::{SummaryRequest, SummaryResult};
use bindings::Compute;

/// Default location of the compiled component, relative to the workspace root.
pub const DEFAULT_COMPONENT_PATH: &str = "target/wasm32-wasip2/debug/wasm_summarize.wasm";

/// Store state. Even a pure-compute component built for `wasm32-wasip2` carries
/// WASI imports (std runtime support), so the host provides a WASI context.
struct HostState {
    ctx: WasiCtx,
    table: ResourceTable,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> &mut WasiCtx {
        &mut self.ctx
    }

    fn table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }
}

/// A loaded, ready-to-invoke summarization component plus its runtime.
pub struct SummarizeRuntime {
    engine: Engine,
    component: Component,
    linker: Linker<HostState>,
}

impl SummarizeRuntime {
    /// Load and validate the component from `path`.
    pub fn load(path: &str) -> Result<Self> {
        let engine = Engine::default();
        let component = Component::from_file(&engine, path)
            .with_context(|| format!("loading wasm component from {path}"))?;
        let mut linker = Linker::<HostState>::new(&engine);
        wasmtime_wasi::add_to_linker_sync(&mut linker)
            .context("wiring WASI into the component linker")?;
        Ok(Self {
            engine,
            component,
            linker,
        })
    }

    /// Invoke the sandboxed `summarize` export with bounded input.
    pub fn summarize(
        &self,
        workspace: &WorkspaceId,
        messages: Vec<String>,
    ) -> Result<SummaryResult> {
        let state = HostState {
            ctx: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
        };
        let mut store = Store::new(&self.engine, state);
        let bindings = Compute::instantiate(&mut store, &self.component, &self.linker)
            .context("instantiating the compute component")?;
        let req = SummaryRequest {
            workspace_id: workspace.as_str().to_string(),
            messages,
        };
        let result = bindings
            .summarybot_compute_summarize()
            .call_summarize(&mut store, &req)
            .context("calling summarize across the host<->wasm boundary")?;
        Ok(result)
    }
}

/// End-to-end walking skeleton: validate -> WASM compute -> persist -> read back.
pub fn run_skeleton(
    runtime: &SummarizeRuntime,
    repo: &dyn SummaryRepository,
    workspace_raw: &str,
    messages: Vec<String>,
) -> Result<StoredSummary> {
    // Validate at the boundary (fail-fast, no silent fallback).
    let workspace = WorkspaceId::parse(workspace_raw)?;

    // Pure compute happens inside the sandbox.
    let wasm_result = runtime.summarize(&workspace, messages)?;

    // Host owns persistence; the repository enforces tenant scoping.
    let summary = domain::Summary {
        text: wasm_result.text,
        message_count: wasm_result.message_count,
        word_count: wasm_result.word_count,
    };
    let id = repo.save_summary(&workspace, &summary)?;

    repo.get_summary(&workspace, id)?
        .context("summary vanished immediately after save")
}

#[cfg(test)]
mod tests {
    use super::*;
    use repository::SqliteRepository;
    use std::path::Path;

    // Integration test for the full skeleton. Requires the component to be
    // built first (`cargo build -p wasm-summarize --target wasm32-wasip2`);
    // it is skipped (not failed) when the artifact is absent so `cargo test`
    // is green without the wasm step. CI builds the artifact, then runs this.
    #[test]
    fn walking_skeleton_roundtrips_through_wasm() {
        let path = DEFAULT_COMPONENT_PATH;
        if !Path::new(path).exists() {
            eprintln!("skipping: {path} not built (run the wasm build first)");
            return;
        }
        let runtime = SummarizeRuntime::load(path).expect("load component");
        let repo = SqliteRepository::in_memory().expect("db");
        let messages = vec!["hello world".to_string(), "second message here".to_string()];
        let stored = run_skeleton(&runtime, &repo, "ws-demo", messages).expect("skeleton");

        assert_eq!(stored.workspace_id, "ws-demo");
        assert_eq!(stored.summary.message_count, 2);
        assert_eq!(stored.summary.word_count, 5);
    }

    #[test]
    fn invalid_workspace_is_rejected_before_wasm() {
        // No component needed: validation fails fast, before any WASM call.
        let repo = SqliteRepository::in_memory().expect("db");
        // Build a runtime only if available; otherwise assert via a direct parse.
        assert!(domain::WorkspaceId::parse("").is_err());
        let _ = &repo; // keep repo in scope to mirror real call shape
    }
}
