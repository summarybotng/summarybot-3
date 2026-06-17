# ADR-134: Real-backend fidelity — the stub blind spot, and channel-fetch diagnostics

> Rewrite-era ADR. Numbering continues from the local ADR set (…, ADR-133).

- **Status**: Accepted (2026-06-17). Channel-fetch diagnostics (§3) shipped;
  the model-default fix (§2.1) shipped; the remaining items in §2 and §4 are
  tracked here as known gaps with a defined remediation.

## Context

A live shake-out against a real OpenRouter key and a real Discord server (the
first time the rewrite talked to *actual* external services, not the in-process
demo stubs) surfaced a cluster of defects that the test suite and prior browser
verification had all missed. They share one root cause, so they're documented
together rather than as scattered bugs.

### Root cause: everything was verified against stubs

The whole test suite and every prior browser check ran on `DemoLlmClient` /
`DemoEmbedder` and never made a real outbound call. The only tests touching the
real HTTP client assert URL-joining and the constructor's endpoint — they never
issue a request, not even to a mock server. So **any behavior that only appears
when talking to a real external service was unverified**:

- the demo LLM client *ignores the model name* and returns canned text, so the
  default model id `"demo"` "worked" everywhere;
- usage/cost fields were never exercised, because the demo client doesn't report
  them;
- a Discord 403 was never seen, because no real Discord call was ever made.

This is the same blind spot, viewed from three angles, as two earlier misses:
the "Report a problem" gap (a UI-walk enumerates *visible* nouns/verbs, not
embedded behaviors) and the v2→v3 parity audit (ADR-133 walked *screens*, not
server-side heuristics). **Invisible-in-the-UI ∧ real-backend-only ⇒ falls
through both our spec style (mechanism-first) and our verification style
(stub + UI-walk).**

## Decision

Fix the concrete defects, and add two cheap nets that catch the *class*.

### §1. Confirmed defects (this shake-out)

| # | Defect | Severity | Status |
|---|--------|----------|--------|
| 1 | LLM model default is the literal `"demo"`, which a real provider rejects with `Llm(InvalidRequest)`. v2 shipped a baked-in Claude default and never asked the user to set a model. | blocked all real summaries | ✅ fixed |
| 2 | `HttpLlmClient` parses only `choices[0].message.content` + `finish_reason`; it never reads `usage`, so `tokens_in/out` and `cost_micros` are always null/0 on a real backend → **per-tenant budgets (ADR-125) and the Spend screen never reflect real spend**. | budgets silently wrong | ❌ open |
| 3 | Channel fetch failures surfaced as a bare `http 403` with no channel name, no cause, no remediation; the sync still reported "completed"; identical errors piled up one-row-per-channel. v2 (ADR-041/097) named the channel, classified the reason, reported coverage, and pre-flighted accessibility. | "isn't getting channel content" with no explanation | ✅ fixed (§3) |
| 4 | Embedder stays the deterministic demo stub unless `LLM_BASE_URL` is set, so with OpenRouter, **semantic/vector search runs on fake embeddings**. | knowledge search quality | ❌ open (documented) |
| 5 | OAuth Connect (Google/Atlassian) verified only to *produce a consent URL*; the callback → token-exchange → refresh round-trip and the actual Confluence/Drive/SMTP delivery were never run live. | delivery untested | ⚠️ known (coverage-map) |
| 6 | `OAUTH_REDIRECT_BASE` defaults to `localhost:8080`, wrong under Codespaces/remote — would break real OAuth callbacks. | config footgun | ⚠️ known |

### §2.1 Model default (fixed)

`resolve_model()` in `crates/api/src/main.rs`: an explicit `LLM_MODEL` always
wins; otherwise, when OpenRouter is the backend, default to
`anthropic/claude-3.5-haiku` (economical current Claude, matching v2's
default-to-economical behavior). A local `LLM_BASE_URL` still needs its
deployment-specific model name; the demo backend keeps `"demo"`.

### §3. Channel-fetch diagnostics (fixed — v2 ADR-041/097 parity)

The "show what's wrong" behavior v2 had, ported to the rewrite:

- **Actionable error mapping.** `discord.rs::discord_error_message` and
  `slack.rs::classify_slack_error` translate the raw status / API error into a
  remediation. A Discord 403 now reads: *"the bot can't read this channel —
  grant it 'View Channel' and 'Read Message History' in Discord … (Discord
  50001: Missing Access)"*. A Slack `not_in_channel` says to `/invite` the bot.
  (Both are pure functions with unit tests; a 403 is distinct from the Message
  Content Intent, which returns 200 with empty content.)
- **Named channels + aggregation.** The sync resolves channel id → name and the
  Errors log records *one* aggregated row per distinct failure listing the
  affected channels by name (`#design`, `#mentors-general`, …) — fixing v2's
  noted "logs pile up with repetitive permission errors".
- **Coverage reporting.** `SyncResponse` gains `channels_total` /
  `channels_failed`; the Source view shows "fetched N from X of Y channels" and,
  when any failed, an amber panel ("25 of 44 channels couldn't be read — fix the
  access below, then sync again") with the per-channel reasons.

- **Pre-flight accessibility (ADR-097).** Discord's channel directory now
  populates `ChannelInfo.accessible` by resolving the bot's effective per-channel
  permissions (`discord.rs::can_read_channel` + `channel_access_map`): base
  `@everyone` + bot-role perms with an `ADMINISTRATOR` bypass, then channel
  overwrites (`@everyone` → union of the bot's role overwrites → member overwrite),
  checking `VIEW_CHANNEL` ∧ `READ_MESSAGE_HISTORY`. The browser already renders a
  "🔒 no access" badge + disabled checkbox for `accessible == false`, so unreadable
  channels are flagged *before* a sync (Slack already did this via `is_member`).
  The permission math is the bug-prone part and is unit-tested; the live wiring is
  best-effort (any roles/member fetch failure leaves `accessible` as `None`).

### §4. The nets that catch the class

1. **A defaults-&-failure-modes pass on the config ADRs** (125 LLM, 126 plugins,
   127 knowledge/embeddings, 132 OAuth): for each resolution ladder, write down
   what happens when a rung resolves *partially* or to *nothing* (e.g. ADR-125
   now documents the per-backend default model + the local "must pin" rule).
2. **A live-backend smoke test** — opt-in, gated behind an env var so CI stays
   hermetic — that hits the configured backend once and asserts HTTP 200,
   non-empty content, **and parsed non-zero token counts**. That single test
   would have caught #1 and #2.

## Consequences

- **Good.** The most common real-world Discord failure (the bot is in the server
  but lacks per-channel read permission) is now self-explanatory and actionable,
  matching v2. The model default makes summaries work with zero model config.
- **Cost.** #2 (usage parsing) and #4 (real embeddings) remain open and are now
  tracked rather than latent; #5/#6 stay as documented live-only follow-ups.
- **Process.** The recurring lesson — invisible + real-backend-only behaviors
  evade stub tests and UI-walk audits — is the reason for the §4 nets.
