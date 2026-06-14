import type {
  Budget,
  DeliveryTestResult,
  Destination,
  KnowledgeHit,
  LlmConfig,
  Plugin,
  TenantPlugin,
  LlmConfigUpdate,
  Schedule,
  ScheduleRun,
  Summary,
  Tenant,
  TokenResponse,
  WhatsappImport,
  ChatCoverage,
  Coverage,
  RetrospectiveResult,
  CoverageGap,
  ImportInvitation,
  WikiPage,
  Job,
  KnowledgeUnit,
  CurationReport,
  ConnectionStatus,
  SourceSync,
  SourceChannel,
  SourceServer,
  Spend,
  AuditEntry,
  Membership,
  Invite,
  IssuedInvite,
} from './types'

const SESSION_KEY = 'sb_session'

export interface Session {
  accessToken: string
  refreshToken: string
  userId: string
  workspaces: string[]
  /** The workspace the dashboard is currently acting on (one of `workspaces`). */
  active?: string
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
  /** Optional live source to fetch before each run (ADR-128): discord | slack. */
  platform?: string | null
  source_id?: string | null
  /** Optional rolling-period digest (ADR-101): weekly | biweekly | monthly. */
  rolling_period?: string | null
  rolling_strategy?: string | null
  rolling_end_day?: number | null
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
    const a = this.session.active
    if (a && this.session.workspaces.includes(a)) return a
    return this.session.workspaces[0] ?? ''
  }

  /** The workspaces this session can act on (granted at login, entitlement-filtered). */
  workspaces(): string[] {
    return this.session.workspaces
  }

  /** Switch the active workspace (must be one of the granted set); persists it. */
  setActiveWorkspace(id: string) {
    if (this.session.workspaces.includes(id)) {
      this.session = { ...this.session, active: id }
      persist(this.session)
    }
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

  /** Retrospective: one summary per week across an imported chat's history. */
  summarizeWeeks(chat: string): Promise<RetrospectiveResult> {
    return this.json<RetrospectiveResult>(
      `/workspaces/${this.ws()}/whatsapp/chats/${encodeURIComponent(chat)}/summarize-weeks`,
      this.body('POST', {}),
    )
  }

  /** Per-chat WhatsApp coverage overview — counts + classified gaps (WHA-017). */
  whatsappChats(): Promise<ChatCoverage[]> {
    return this.json<ChatCoverage[]>(`/workspaces/${this.ws()}/whatsapp/chats`)
  }

  /** One chat's merged coverage: gaps + contributors + invitations (WHA-016/018/019). */
  whatsappCoverage(chat: string): Promise<Coverage> {
    return this.json<Coverage>(
      `/workspaces/${this.ws()}/whatsapp/chats/${encodeURIComponent(chat)}/coverage`,
    )
  }

  /** Open a scoped import invitation for a date range (WHA-019). */
  createInvitation(
    chat: string,
    body: { range_start: number; range_end: number; kind: CoverageGap['kind']; note?: string },
  ): Promise<ImportInvitation> {
    return this.json<ImportInvitation>(
      `/workspaces/${this.ws()}/whatsapp/chats/${encodeURIComponent(chat)}/invitations`,
      this.body('POST', body),
    )
  }

  /** Withdraw a standing import invitation (WHA-019). */
  async cancelInvitation(chat: string, id: string): Promise<void> {
    await this.json<unknown>(
      `/workspaces/${this.ws()}/whatsapp/chats/${encodeURIComponent(
        chat,
      )}/invitations/${encodeURIComponent(id)}/cancel`,
      { method: 'POST' },
    )
  }

  // --- knowledge: semantic search (ADR-127) ---

  searchKnowledge(q: string, k = 10): Promise<KnowledgeHit[]> {
    const p = new URLSearchParams({ q, k: String(k) })
    return this.json<KnowledgeHit[]>(`/workspaces/${this.ws()}/wiki/search?${p}`)
  }

  // --- wiki synthesis (WIK-001..003) ---

  /** List synthesized wiki pages (v1: a single "knowledge-base" page). */
  listWikiPages(): Promise<WikiPage[]> {
    return this.json<WikiPage[]>(`/workspaces/${this.ws()}/wiki/pages`)
  }

  /** (Re)generate the knowledge-base page from this workspace's units. */
  synthesizeWiki(): Promise<WikiPage> {
    return this.json<WikiPage>(`/workspaces/${this.ws()}/wiki/synthesize`, this.body('POST', {}))
  }

  /** Background / long-running jobs with progress (ADR-040). */
  listJobs(limit = 50): Promise<Job[]> {
    return this.json<Job[]>(`/workspaces/${this.ws()}/jobs?limit=${limit}`)
  }

  /** Raw knowledge units with provenance (ADR-063 raw-updates view). */
  listUnits(k = 200): Promise<KnowledgeUnit[]> {
    return this.json<KnowledgeUnit[]>(`/workspaces/${this.ws()}/wiki/units?k=${k}`)
  }

  /** AI wiki curator: advisory health report — duplicate clusters + stale units. */
  curateWiki(staleDays = 90): Promise<CurationReport> {
    return this.json<CurationReport>(
      `/workspaces/${this.ws()}/wiki/curate?stale_days=${staleDays}`,
      this.body('POST', {}),
    )
  }

  /** Apply the curator: prune duplicate units (provenance preserved). */
  pruneWiki(): Promise<{ pruned: number }> {
    return this.json<{ pruned: number }>(
      `/workspaces/${this.ws()}/wiki/curate/prune`,
      this.body('POST', {}),
    )
  }

  /** Summarization cost analytics over the last `days` (ADR-125). */
  spend(days = 30): Promise<Spend> {
    return this.json<Spend>(`/workspaces/${this.ws()}/spend?days=${days}`)
  }

  /** Security/admin audit events for this workspace's tenant (WSP-014). */
  listAudit(limit = 100): Promise<AuditEntry[]> {
    return this.json<AuditEntry[]>(`/workspaces/${this.ws()}/audit?limit=${limit}`)
  }

  // --- live source ingestion: Discord / Slack (ADR-128) ---

  /** Token + support status for a live platform (discord|slack). */
  connectionStatus(platform: string): Promise<ConnectionStatus> {
    return this.json(`/workspaces/${this.ws()}/connections/${platform}`)
  }

  setConnectionToken(platform: string, token: string): Promise<ConnectionStatus> {
    return this.json(`/workspaces/${this.ws()}/connections/${platform}/token`, this.body('PUT', { token }))
  }

  clearConnectionToken(platform: string): Promise<void> {
    return this.json(`/workspaces/${this.ws()}/connections/${platform}/token`, { method: 'DELETE' })
  }

  /** Fetch a source's recent messages into the store. `scopeId` is the Discord
   *  guild id (Slack ignores it). */
  syncSource(
    platform: string,
    lookbackSecs: number,
    scopeId?: string,
    channels: string[] = [],
  ): Promise<SourceSync> {
    return this.json<SourceSync>(
      `/workspaces/${this.ws()}/connections/${platform}/sync`,
      this.body('POST', { scope_id: scopeId || null, lookback_secs: lookbackSecs, channels }),
    )
  }

  /** List the servers (Discord guilds) the bot token can reach (WSP-006). */
  sourceServers(platform: string): Promise<SourceServer[]> {
    return this.json<SourceServer[]>(`/workspaces/${this.ws()}/connections/${platform}/servers`)
  }

  /** Browse a source's channels (grouped by category where it has them, WSP-006). */
  sourceChannels(platform: string, scopeId?: string): Promise<SourceChannel[]> {
    const p = scopeId ? `?scope_id=${encodeURIComponent(scopeId)}` : ''
    return this.json<SourceChannel[]>(`/workspaces/${this.ws()}/connections/${platform}/channels${p}`)
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

  // --- tenant delivery plugins (enablement + credentials/connect, ADR-126) ---

  listTenantPlugins(tenant: string): Promise<TenantPlugin[]> {
    return this.json<TenantPlugin[]>(`/tenants/${encodeURIComponent(tenant)}/plugins`)
  }

  setTenantPlugin(
    tenant: string,
    kind: string,
    body: { enabled: boolean; config: Record<string, string> },
  ): Promise<TenantPlugin> {
    return this.json<TenantPlugin>(
      `/tenants/${encodeURIComponent(tenant)}/plugins/${encodeURIComponent(kind)}`,
      this.body('PUT', body),
    )
  }

  clearTenantPlugin(tenant: string, kind: string): Promise<void> {
    return this.json<void>(
      `/tenants/${encodeURIComponent(tenant)}/plugins/${encodeURIComponent(kind)}`,
      { method: 'DELETE' },
    )
  }

  /** Start an OAuth connect for a plugin (e.g. Google Drive); returns the URL. */
  connectPlugin(tenant: string, kind: string): Promise<{ url: string }> {
    return this.json<{ url: string }>(
      `/tenants/${encodeURIComponent(tenant)}/plugins/${encodeURIComponent(kind)}/connect`,
      { method: 'POST' },
    )
  }

  // --- tenant members + invites (RBAC admin) ---

  listMembers(tenant: string): Promise<Membership[]> {
    return this.json<Membership[]>(`/tenants/${tenant}/members`)
  }

  setMemberRole(tenant: string, user: string, role: string): Promise<Membership> {
    return this.json<Membership>(
      `/tenants/${tenant}/members/${encodeURIComponent(user)}`,
      this.body('PUT', { role }),
    )
  }

  removeMember(tenant: string, user: string): Promise<void> {
    return this.json<void>(`/tenants/${tenant}/members/${encodeURIComponent(user)}`, {
      method: 'DELETE',
    })
  }

  listInvites(tenant: string): Promise<Invite[]> {
    return this.json<Invite[]>(`/tenants/${tenant}/invites`)
  }

  /** Issue an invite; the raw `token` is returned exactly once. */
  createInvite(tenant: string, email: string, role: string): Promise<IssuedInvite> {
    return this.json<IssuedInvite>(`/tenants/${tenant}/invites`, this.body('POST', { email, role }))
  }

  /** Revoke a listed invite by its `token_hash` (the raw token is shown once). */
  revokeInvite(tenant: string, tokenHash: string): Promise<void> {
    return this.json<void>(
      `/tenants/${tenant}/invites/revoke`,
      this.body('POST', { token_hash: tokenHash }),
    )
  }
}
