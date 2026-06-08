# Always browser-check the UX

After any change that touches the dashboard SPA (`web/`), **visually verify the
running UI in a real browser** — don't rely on build + curl alone.

## Why

The Delivery tab (webhook delivery) was shipped having only confirmed the
backend via `curl` and that the bundle compiled (`tsc` + `vite build`). Nobody
had actually looked at the rendered page. "It builds" and "the API returns 200"
do not prove the view renders, the layout holds, or the buttons do what they
should.

## How to apply

After the build, drive the app in a browser (the `browser` skill / agent-browser
works headless in the Codespace):

1. Open the dashboard at `http://127.0.0.1:8080`.
2. Dev login: provider `email`, any email, workspace `ws-demo`.
3. Navigate to the changed view; take a screenshot/snapshot.
4. Exercise the key interaction (submit a form, add/test/remove an item) and
   confirm the result and any success/error messaging.
5. Report what you actually saw — not just that it compiled.

Treat this as part of "verified," alongside `cargo test` / `clippy` / `fmt` and
`npm run build`.

Run config for the API + local LLM is in the team onboarding / dev notes
(Tailscale + Ollama on the Mac mini, `--features http-llm`, `LLM_BASE_URL`,
`LLM_CONTEXT_TOKENS`).
