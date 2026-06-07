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
