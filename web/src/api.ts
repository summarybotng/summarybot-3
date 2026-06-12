import type {
  Budget,
  DeliveryTestResult,
  Destination,
  KnowledgeHit,
  LlmConfig,
  Plugin,
  LlmConfigUpdate,
  Schedule,
  ScheduleRun,
  Summary,
  Tenant,
  TokenResponse,
  WhatsappImport,
} from './types'

const SESSION_KEY = 'sb_session'

export interface Session {
  accessToken: string
  refreshToken: string
  userId: string
  workspaces: string[]
}

export class ApiError extends Error {
  status: number
  constructor(status: number, message: string) {
    super(message)
    this.status = status
  }
}

export function loadSession(): Session | null {
  const raw = localStorage.getItem(SESSION_KEY)
  if (!raw) return null
  try {
    return JSON.parse(raw) as Session
  } catch {
    return null
  }
}

function persist(s: Session | null) {
  if (s) localStorage.setItem(SESSION_KEY, JSON.stringify(s))
  else localStorage.removeItem(SESSION_KEY)
}

/** Exchange dev provider claims for a session (the OAuth redirect is a later seam). */
export async function login(
  provider: string,
  subject: string,
  email: string | null,
  workspaces: string[],
): Promise<Session> {
  const res = await fetch('/auth/login', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ provider, subject, email, workspaces }),
  })
  if (!res.ok) throw new ApiError(res.status, 'login failed')
  const t = (await res.json()) as TokenResponse
  return {
    accessToken: t.access_token,
    refreshToken: t.refresh_token,
    userId: t.user_id,
    workspaces,
  }
}

export interface CreateScheduleBody {
  schedule_type: string
  hour?: number
  minute?: number
  timezone?: string
  channel?: string | null
  lookback_secs?: number
}

/** An authenticated API client bound to a session, with refresh-on-401. */
export class Client {
  private session: Session
  private onLogout: () => void

  constructor(session: Session, onLogout: () => void) {
    this.session = session
    this.onLogout = onLogout
    persist(session)
  }

  ws(): string {
    return this.session.workspaces[0] ?? ''
  }

  token(): string {
    return this.session.accessToken
  }

  eventsUrl(): string {
    return `/workspaces/${this.ws()}/events`
  }

  private async raw(path: string, init: RequestInit, retry = true): Promise<Response> {
    const headers = new Headers(init.headers)
    headers.set('authorization', `Bearer ${this.session.accessToken}`)
    const res = await fetch(path, { ...init, headers })
    if (res.status === 401 && retry) {
      if (await this.tryRefresh()) return this.raw(path, init, false)
      this.onLogout()
    }
    return res
  }

  private async tryRefresh(): Promise<boolean> {
    const res = await fetch('/auth/refresh', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        refresh_token: this.session.refreshToken,
        workspaces: this.session.workspaces,
      }),
    })
    if (!res.ok) return false
    const t = (await res.json()) as TokenResponse
    this.session = { ...this.session, accessToken: t.access_token, refreshToken: t.refresh_token }
    persist(this.session)
    return true
  }

  private async json<T>(path: string, init: RequestInit = {}): Promise<T> {
    const res = await this.raw(path, init)
    if (!res.ok) throw new ApiError(res.status, await res.text())
    if (res.status === 204) return undefined as T
    return (await res.json()) as T
  }

  private body(method: string, data: unknown): RequestInit {
    return { method, headers: { 'content-type': 'application/json' }, body: JSON.stringify(data) }
  }

  logout() {
    fetch('/auth/logout', {
      method: 'POST',
      headers: { authorization: `Bearer ${this.session.accessToken}` },
    }).catch(() => {})
    persist(null)
  }

  tenant(): Promise<Tenant> {
    return this.json<Tenant>('/tenant')
  }

  listSummaries(
    q: { q?: string; participant?: string; tag?: string; includeArchived?: boolean } = {},
  ): Promise<Summary[]> {
    const p = new URLSearchParams()
    if (q.q) p.set('q', q.q)
    if (q.participant) p.set('participant', q.participant)
    if (q.tag) p.set('tag', q.tag)
    if (q.includeArchived) p.set('include_archived', 'true')
    const qs = p.toString()
    return this.json<Summary[]>(`/workspaces/${this.ws()}/summaries${qs ? `?${qs}` : ''}`)
  }

  createSummary(messages: string[]): Promise<Summary> {
    return this.json<Summary>(`/workspaces/${this.ws()}/summaries`, this.body('POST', { messages }))
  }

  getSummary(id: string): Promise<Summary> {
    return this.json<Summary>(`/workspaces/${this.ws()}/summaries/${id}`)
  }

  deleteSummary(id: string): Promise<void> {
    return this.json<void>(`/workspaces/${this.ws()}/summaries/${id}`, { method: 'DELETE' })
  }

  setPinned(id: string, pinned: boolean): Promise<Summary> {
    return this.json<Summary>(
      `/workspaces/${this.ws()}/summaries/${id}/${pinned ? 'pin' : 'unpin'}`,
      { method: 'POST' },
    )
  }

  setArchived(id: string, archived: boolean): Promise<Summary> {
    return this.json<Summary>(
      `/workspaces/${this.ws()}/summaries/${id}/${archived ? 'archive' : 'unarchive'}`,
      { method: 'POST' },
    )
  }

  listSchedules(): Promise<Schedule[]> {
    return this.json<Schedule[]>(`/workspaces/${this.ws()}/schedules`)
  }

  createSchedule(body: CreateScheduleBody): Promise<Schedule> {
    return this.json<Schedule>(`/workspaces/${this.ws()}/schedules`, this.body('POST', body))
  }

  triggerSchedule(id: string): Promise<{ produced: boolean; summary: Summary | null }> {
    return this.json(`/workspaces/${this.ws()}/schedules/${id}/run`, { method: 'POST' })
  }

  setScheduleEnabled(id: string, enabled: boolean): Promise<Schedule> {
    return this.json<Schedule>(
      `/workspaces/${this.ws()}/schedules/${id}/${enabled ? 'resume' : 'pause'}`,
      { method: 'POST' },
    )
  }

  deleteSchedule(id: string): Promise<void> {
    return this.json<void>(`/workspaces/${this.ws()}/schedules/${id}`, { method: 'DELETE' })
  }

  listRuns(id: string): Promise<ScheduleRun[]> {
    return this.json<ScheduleRun[]>(`/workspaces/${this.ws()}/schedules/${id}/runs`)
  }

  /**
   * Summarize a channel's recent messages now, without leaving a schedule
   * behind: create an ephemeral schedule scoped to the channel, trigger it, then
   * delete it. Reuses the tested schedule endpoints.
   */
  async summarizeChannelNow(
    chat: string,
    lookbackSecs: number,
  ): Promise<{ produced: boolean; summary: Summary | null }> {
    const sched = await this.createSchedule({
      schedule_type: 'daily',
      hour: 9,
      timezone: 'UTC',
      channel: chat,
      lookback_secs: lookbackSecs,
    })
    try {
      return await this.triggerSchedule(sched.id)
    } finally {
      await this.deleteSchedule(sched.id).catch(() => {})
    }
  }

  /** Upload a WhatsApp export (.zip or _chat.txt) as the raw request body. */
  importWhatsapp(
    chat: string,
    tz: string,
    dateOrder: string,
    file: File,
  ): Promise<WhatsappImport> {
    const p = new URLSearchParams({ chat, tz })
    if (dateOrder) p.set('date_order', dateOrder)
    return this.json<WhatsappImport>(`/workspaces/${this.ws()}/whatsapp/imports?${p}`, {
      method: 'POST',
      body: file,
    })
  }

  // --- knowledge: semantic search (ADR-127) ---

  searchKnowledge(q: string, k = 10): Promise<KnowledgeHit[]> {
    const p = new URLSearchParams({ q, k: String(k) })
    return this.json<KnowledgeHit[]>(`/workspaces/${this.ws()}/wiki/search?${p}`)
  }

  // --- per-workspace summarization settings (SUM-007) ---

  getWorkspaceSettings(): Promise<{ summary_instructions: string | null }> {
    return this.json(`/workspaces/${this.ws()}/settings`)
  }

  setWorkspaceSettings(summary_instructions: string | null): Promise<{
    summary_instructions: string | null
  }> {
    return this.json(`/workspaces/${this.ws()}/settings`, this.body('PUT', { summary_instructions }))
  }

  // --- delivery destinations (DSH-010/011) ---

  listDestinations(): Promise<Destination[]> {
    return this.json<Destination[]>(`/workspaces/${this.ws()}/destinations`)
  }

  /** Sink plugins available in this build, with their config schema (ADR-126). */
  listPlugins(): Promise<Plugin[]> {
    return this.json<Plugin[]>(`/workspaces/${this.ws()}/destinations/plugins`)
  }

  /** Add a destination of `kind` with its config (stored encrypted, never returned). */
  addDestination(kind: string, config: Record<string, string>): Promise<Destination> {
    return this.json<Destination>(
      `/workspaces/${this.ws()}/destinations`,
      this.body('POST', { kind, config }),
    )
  }

  deleteDestination(id: string): Promise<void> {
    return this.json<void>(`/workspaces/${this.ws()}/destinations/${id}`, { method: 'DELETE' })
  }

  /** Send a sample payload to a destination to confirm it works. */
  testDestination(id: string): Promise<DeliveryTestResult> {
    return this.json<DeliveryTestResult>(`/workspaces/${this.ws()}/destinations/${id}/test`, {
      method: 'POST',
    })
  }

  // --- tenancy (control plane) ---

  /** Provision a tenant; the caller becomes its Owner. */
  provisionTenant(id: string): Promise<Tenant> {
    return this.json<Tenant>('/tenants', this.body('POST', { id, name: id }))
  }

  getLlmConfig(tenant: string): Promise<LlmConfig> {
    return this.json<LlmConfig>(`/tenants/${tenant}/llm-config`)
  }

  setLlmConfig(tenant: string, cfg: LlmConfigUpdate): Promise<LlmConfig> {
    return this.json<LlmConfig>(`/tenants/${tenant}/llm-config`, this.body('PUT', cfg))
  }

  clearLlmConfig(tenant: string): Promise<void> {
    return this.json<void>(`/tenants/${tenant}/llm-config`, { method: 'DELETE' })
  }

  getBudget(tenant: string): Promise<Budget> {
    return this.json<Budget>(`/tenants/${tenant}/budget`)
  }

  setBudget(tenant: string, limit_micros: number, period_secs: number): Promise<Budget> {
    return this.json<Budget>(
      `/tenants/${tenant}/budget`,
      this.body('PUT', { limit_micros, period_secs }),
    )
  }

  clearBudget(tenant: string): Promise<void> {
    return this.json<void>(`/tenants/${tenant}/budget`, { method: 'DELETE' })
  }
}
