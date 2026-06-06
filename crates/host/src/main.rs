//! Phase 0 walking-skeleton demo. Runs one trivial request through the full
//! path: native host -> sandboxed WASM compute -> repository -> SQLite.
//!
//!   cargo build -p wasm-summarize --target wasm32-wasip2
//!   cargo run -p host                 # uses the default component path
//!   cargo run -p host -- <path.wasm>  # or point at an explicit component

use anyhow::Result;
use host::{run_skeleton, SummarizeRuntime, DEFAULT_COMPONENT_PATH};
use repository::SqliteRepository;

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_COMPONENT_PATH.to_string());

    let runtime = SummarizeRuntime::load(&path)?;
    let repo = SqliteRepository::in_memory()?;

    let messages = vec![
        "deploy went out at noon and the dashboard is green".to_string(),
        "follow up: confirm the rollback runbook is current".to_string(),
        "thanks everyone".to_string(),
    ];

    let stored = run_skeleton(&runtime, &repo, "ws-demo", messages)?;

    println!("walking skeleton OK");
    println!("  stored id      : {}", stored.id);
    println!("  workspace      : {}", stored.workspace_id);
    println!("  message_count  : {}", stored.summary.message_count);
    println!("  word_count     : {}", stored.summary.word_count);
    println!("  summary        : {}", stored.summary.text);
    Ok(())
}
