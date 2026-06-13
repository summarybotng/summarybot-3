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

export interface ActionItem {
  text: string
  assignee: string | null
}

export interface Citation {
  message_id: string
  quote: string | null
}

export interface Summary {
  id: string
  channel_id: string | null
  model: string
  cost_micros: number
  degraded: boolean
  /** Coherence-gate grounded score in [0,1]; null if unassessed (COH-001). */
  coherence_score: number | null
  pinned: boolean
  archived: boolean
  tags: string[]
  created_at: number
  text: string
  key_points: string[]
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
