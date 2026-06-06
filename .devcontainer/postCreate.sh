#!/usr/bin/env bash
# postCreate: restore ephemeral context that lives OUTSIDE the committed repo.
# Codespaces/devcontainers are disposable — anything not in git must be re-fetched here.
set -euo pipefail

REF_DIR="/workspaces/summarybot-ng-reference"
REF_REPO="https://github.com/summarybotng/summarybot-ng/"

# 1. Re-clone the OLD project as read-only reference material (ADRs, technical-debt.md,
#    proven algorithm files). Used for the Rust/WASM rewrite — see docs/REWRITE-CONTEXT.md.
if [ ! -d "$REF_DIR/.git" ]; then
  echo "[postCreate] Cloning reference repo -> $REF_DIR"
  git clone --depth 1 "$REF_REPO" "$REF_DIR"
else
  echo "[postCreate] Reference repo already present at $REF_DIR"
fi

# 2. Rust toolchain sanity (wasm target for the rewrite).
if command -v rustup >/dev/null 2>&1; then
  rustup target add wasm32-unknown-unknown || true
fi

echo "[postCreate] Done. Reference material at $REF_DIR (NOT tracked by git)."
