// Shapes mirroring the API DTOs (see crates/api). Kept hand-written and small;
// a generated client from /openapi.json is a later upgrade.

export interface TokenResponse {
  access_token: string
  refresh_token: string
  user_id: string
  access_expires_at: number
}

export interface Tenant {
  id: string
  name: string
  subdomain: string | null
  custom_domain: string | null
}

/** A tenant the signed-in user belongs to, with their role in it (TEN-001). */
export interface MyTenant {
  id: string
  name: string
  role: string
}

export interface ActionItem {
  text: string
  assignee: string | null
}

export interface Citation {
  message_id: string
  quote: string | null
}

/** One source reference behind a grounded claim (ADR-004 §2.1). */
export interface Reference {
  message_id: string
  author_name: string
  timestamp: number
  /** 1-based position in the summarized window. */
  position: number
  snippet: string
}

/** A grounded key point (ADR-004): claim text + confidence + its references. */
export interface KeyPoint {
  text: string
  confidence: number
  references: Reference[]
}

export interface Summary {
  id: string
  channel_id: string | null
  model: string
  cost_micros: number
  degraded: boolean
  /** Coherence-gate grounded score in [0,1]; null if unassessed (COH-001). */
  coherence_score: number | null
  /** Wall-clock latency of the producing run, in ms (ADR-106 metadata). */
  latency_ms: number
  /** Input/output tokens the producing run consumed (ADR-106 metadata). */
  input_tokens: number
  output_tokens: number
  /** Covered message-time window (ADR-133); zero-width for ad-hoc pasted text. */
  period_start: number
  period_end: number
  /** Built-in perspective that steered this summary (ADR-133 §B), if any. */
  perspective: string | null
  pinned: boolean
  archived: boolean
  tags: string[]
  created_at: number
  text: string
  key_points: KeyPoint[]
  action_items: ActionItem[]
  technical_terms: string[]
  participants: string[]
  citations: Citation[]
}

export interface Schedule {
  id: string
  schedule_type: string
  hour: number
  minute: number
  days: number[]
  day_of_month: number
  timezone: string
  enabled: boolean
  channel: string | null
  lookback_secs: number
  platform: string | null
  source_id: string | null
  rolling_period: string | null
  rolling_strategy: string | null
  rolling_end_day: number | null
  /** Destination ids this schedule is pinned to (ADR-014); empty = all enabled. */
  destinations: string[]
  /** Steering options (ADR-133 §B). */
  prompt_template_id: string | null
  perspective: string | null
  title_template: string | null
  enable_continuity: boolean
  next_run: number
  consecutive_failures: number
}

export interface ScheduleRun {
  id: number
  schedule_id: string
  ran_at: number
  status: string
  detail: string | null
  manual: boolean
}

export interface LiveEvent {
  workspace_id: string
  kind: string
  summary_id?: string
}

export interface LlmConfig {
  base_url: string | null
  model: string | null
  has_key: boolean
}

/** PUT body for LLM config; api_key is tri-state (omit=keep, null=clear, value=set). */
export interface LlmConfigUpdate {
  base_url?: string | null
  model?: string | null
  api_key?: string | null
}

export interface Budget {
  configured: boolean
  limit_micros: number
  period_secs: number
  spent_micros: number
  remaining_micros: number
  period_start: number
}

export interface WhatsappImport {
  chat_id: string
  /** Whether chat_id was auto-detected from the export (no chat supplied). */
  chat_auto_detected: boolean
  format: string
  messages: number
  stored: number
  duplicates: number
  new_participants: number
  date_start: number | null
  date_end: number | null
}

/** A classified hole in a chat's coverage (WHA-016/017; ADR-121). */
export interface CoverageGap {
  start: number
  end: number
  /** `before_join` | `between_imports` | `after_last`. */
  kind: 'before_join' | 'between_imports' | 'after_last'
  /** Whether a member could plausibly export this range to fill it. */
  can_fill: boolean
}

/** One gap period in the workspace coverage view (ADR-133). */
export interface WorkspaceCoverageGap {
  start: number
  end: number
  /** `before_start` | `between` | `after_last`. */
  kind: string
}

/** One channel's coverage row (ADR-133). */
export interface ChannelCoverage {
  channel_id: string
  earliest_content: number
  latest_content: number
  message_count: number
  summary_count: number
  /** Covered seconds / content span, as a percent (can exceed 100 on overlap). */
  coverage_percent: number
  gap_count: number
  gaps: WorkspaceCoverageGap[]
}

/** Server-wide + per-channel summary coverage (ADR-133). */
export interface WorkspaceCoverage {
  total_coverage_percent: number
  total_gaps: number
  total_channels: number
  covered_channels: number
  total_summaries: number
  earliest_content: number | null
  latest_content: number | null
  channels: ChannelCoverage[]
}

/** A built-in summary perspective (ADR-133). */
export interface Perspective {
  id: string
  label: string
}

/** A saved named prompt template (ADR-133). */
export interface PromptTemplate {
  id: string
  name: string
  content: string
  based_on: string | null
  usage_count: number
  created_at: number
  updated_at: number
}

/** Prompts screen payload: built-in perspectives + workspace templates. */
export interface Prompts {
  perspectives: Perspective[]
  templates: PromptTemplate[]
}

/** One recorded operational error (ADR-133 A3). */
export interface OperationalError {
  id: string
  operation: string
  error_class: string
  /** `error` | `warning`. */
  severity: string
  channel_id: string | null
  message: string
  resolved: boolean
  created_at: number
}

/** Operational error log payload (ADR-133 A3). */
export interface Errors {
  unresolved: number
  errors: OperationalError[]
}

/** Outcome of a retrospective by-week summarize run. */
export interface RetrospectiveResult {
  produced: number
  weeks_empty: number
  summary_ids: string[]
  truncated: boolean
}

/** One member's contribution to a chat (WHA-018). */
export interface Contribution {
  uploader: string
  import_count: number
  message_count: number
  earliest: number
  latest: number
}

/** A persisted scoped import invitation (WHA-019). */
export interface ImportInvitation {
  id: string
  chat_id: string
  range_start: number
  range_end: number
  kind: CoverageGap['kind']
  note: string
  /** `open` | `fulfilled` | `cancelled`. */
  status: 'open' | 'fulfilled' | 'cancelled'
  created_by: string
  created_at: number
  fulfilled_by: string | null
  fulfilled_at: number | null
}

/** One chat's merged coverage picture, with contributors + standing asks. */
export interface Coverage {
  chat_id: string
  earliest: number | null
  latest: number | null
  covered_secs: number
  gaps: CoverageGap[]
  contributors: Contribution[]
  invitations: ImportInvitation[]
}

/** A chat in the workspace coverage overview. */
export interface ChatCoverage {
  chat_id: string
  import_count: number
  message_count: number
  coverage: Coverage
}

export interface Destination {
  id: string
  kind: string
  enabled: boolean
  /** Non-secret summary of the config; secret fields are never returned. */
  hint: string | null
  /** ADR-108: deliver here on every rolling run, not just at finalize. */
  rolling_deliver_intermediate: boolean
}

export interface KnowledgeHit {
  id: string
  summary_id: string
  kind: string
  text: string
  source_ids: string[]
  score: number
}

export interface WikiPage {
  slug: string
  title: string
  content_md: string
  unit_count: number
  updated_at: number
}

/** A background / long-running job (ADR-040). */
export interface Job {
  id: string
  job_type: string
  status: string
  progress_current: number
  progress_total: number
  cost_micros: number
  failure_reason: string | null
  created_at: number
  updated_at: number
  // ADR-133 §B context.
  scope: string | null
  schedule_name: string | null
  summary_ids: string[]
  date_start: number
  date_end: number
  started_at: number | null
  completed_at: number | null
  creation_source: string | null
  pause_reason: string | null
}

/** A raw knowledge unit with its source provenance (ADR-063). */
export interface KnowledgeUnit {
  id: string
  kind: string
  text: string
  summary_id: string
  source_ids: string[]
  created_at: number
}

/** A near-identical group from the AI wiki curator (CUR-*). */
export interface DuplicateCluster {
  canonical_id: string
  kind: string
  text: string
  duplicate_ids: string[]
}

/** A knowledge unit flagged old enough to review. */
export interface StaleUnit {
  id: string
  kind: string
  text: string
  age_secs: number
}

/** The AI wiki curator's advisory health report. */
export interface CurationReport {
  total_units: number
  embedded_units: number
  redundant_count: number
  duplicate_clusters: DuplicateCluster[]
  stale: StaleUnit[]
}

export interface ConnectionStatus {
  token_set: boolean
  supported: boolean
}

export interface Spend {
  total_micros: number
  summary_count: number
  recent_micros: number
  recent_days: number
  by_model: { model: string; count: number; cost_micros: number }[]
}

export interface AuditEntry {
  ts: number
  actor: string | null
  action: string
  detail: string
}

export interface Membership {
  tenant_id: string
  user_id: string
  role: string
}

export interface Invite {
  token_hash: string
  tenant_id: string
  email: string
  role: string
  created_at: number
  expires_at: number
  status: string
}

export interface IssuedInvite {
  token: string
  token_hash: string
  email: string
  role: string
  expires_at: number
}

export interface SourceSync {
  channel_ids: string[]
  fetched: number
  stored: number
  errors: { channel: string; message: string }[]
}

/** A server (Discord guild) the bot token can reach (WSP-006). */
export interface SourceServer {
  id: string
  name: string
}

/** A browsable channel in a source's directory (WSP-006). */
export interface SourceChannel {
  id: string
  name: string
  /** Discord category name; null for Slack / uncategorized. */
  category: string | null
  /** Discord category id — the scope target for a category schedule (ADR-011). */
  category_id: string | null
}

export interface PluginField {
  name: string
  label: string
  secret: boolean
  required: boolean
}

/** A delivery sink plugin available in this build (ADR-126). */
export interface Plugin {
  id: string
  display_name: string
  fields: PluginField[]
}

export interface DeliveryTestResult {
  ok: boolean
  detail: string | null
}

/** A delivery plugin's tenant-level state (ADR-126 two-layer model). */
export interface TenantPlugin {
  kind: string
  display_name: string
  /** Fields the tenant configures once (credentials/connection). */
  tenant_fields: PluginField[]
  /** Fields a workspace sets per destination (the target) — shown for context. */
  workspace_fields: PluginField[]
  enabled: boolean
  /** All required tenant fields present (true when there are none). */
  configured: boolean
  /** OAuth plugins: a refresh token has been captured via Connect. */
  connected: boolean
  /** Whether this plugin uses an OAuth "Connect" button vs typed config. */
  supports_connect: boolean
  /** Non-secret one-line summary of the configured credentials. */
  hint: string | null
  /** ADR-131: a platform operator has disabled this plugin for the tenant
   * (read-only for tenant admins; the operator toggles it). */
  operator_disabled: boolean
}
