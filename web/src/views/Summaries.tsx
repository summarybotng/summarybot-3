import { useCallback, useEffect, useState } from 'react'
import { useAuth } from '../auth'
import { useSSE } from '../useSSE'
import { Markdown } from '../components/Markdown'
import type { Summary } from '../types'

function when(ts: number): string {
  return new Date(ts * 1000).toLocaleString()
}

/// Human-readable scope of a summary for the metadata panel: an explicit channel,
/// or the scope implied by its tags (all-channels / a Discord category), else
/// workspace-wide.
function scopeLabel(s: Summary): string {
  if (s.channel_id) return `#${s.channel_id}`
  const cat = s.tags.find((t) => t.startsWith('category:'))
  if (cat) return `category ${cat.slice('category:'.length)}`
  if (s.tags.includes('all-channels')) return 'all channels'
  return 'workspace-wide'
}

/// The "kind" of a summary, derived from its tags (ADR-133 §B): rolling digest,
/// retrospective by-week, a scheduled run, or an ad-hoc on-demand summary. This
/// subsumes the v2 rolling/granularity facets without a per-summary schema.
function kindOf(s: Summary): 'rolling' | 'retrospective' | 'scheduled' | 'adhoc' {
  if (s.tags.some((t) => t.startsWith('rolling-'))) return 'rolling'
  if (s.tags.includes('retrospective-weekly')) return 'retrospective'
  if (s.id.startsWith('sum_sch_') || s.id.includes('_sch_')) return 'scheduled'
  return 'adhoc'
}

export function Summaries() {
  const { client } = useAuth()
  const [items, setItems] = useState<Summary[]>([])
  const [q, setQ] = useState('')
  const [participant, setParticipant] = useState('')
  const [tag, setTag] = useState('')
  const [includeArchived, setIncludeArchived] = useState(false)
  const [draft, setDraft] = useState('')
  const [busy, setBusy] = useState(false)
  const [expanded, setExpanded] = useState<string | null>(null)
  const [liveFlash, setLiveFlash] = useState<string | null>(null)
  // Enabled destinations for on-demand publish (ADR-133 D2).
  const [dests, setDests] = useState<{ id: string; kind: string }[]>([])
  const [publishMsg, setPublishMsg] = useState<string | null>(null)
  // Client-side facets over the loaded set (ADR-133 §B).
  const [kind, setKind] = useState('all')
  const [persp, setPersp] = useState('all')
  const [view, setView] = useState<'list' | 'calendar'>('list')

  const load = useCallback(
    async (filters?: { q?: string; participant?: string; tag?: string; includeArchived?: boolean }) => {
      if (!client) return
      setItems(
        await client.listSummaries({
          q: filters?.q || undefined,
          participant: filters?.participant || undefined,
          tag: filters?.tag || undefined,
          includeArchived: filters?.includeArchived || undefined,
        }),
      )
    },
    [client],
  )

  const applyFilters = useCallback(
    () => load({ q, participant, tag, includeArchived }),
    [load, q, participant, tag, includeArchived],
  )

  useEffect(() => {
    void load()
  }, [load])

  useEffect(() => {
    if (!client) return
    client
      .listDestinations()
      .then((d) => setDests(d.filter((x) => x.enabled).map((x) => ({ id: x.id, kind: x.kind }))))
      .catch(() => {})
  }, [client])

  async function publishTo(id: string, destId: string) {
    if (!client || !destId) return
    const r = await client.publishSummary(id, destId)
    setPublishMsg(r.ok ? 'Published ✓' : `Publish failed: ${r.detail ?? 'error'}`)
    setTimeout(() => setPublishMsg(null), 2500)
  }

  // Live updates: prepend on create, drop on delete (deduped by id).
  useSSE(client?.eventsUrl() ?? '', client?.token() ?? '', !!client, (kind, data) => {
    let ev: { summary_id?: string }
    try {
      ev = JSON.parse(data)
    } catch {
      return
    }
    if (kind === 'summary.created' && ev.summary_id && client) {
      const id = ev.summary_id
      client
        .getSummary(id)
        .then((s) => {
          setItems((cur) => (cur.some((x) => x.id === s.id) ? cur : [s, ...cur]))
          setLiveFlash(id)
          setTimeout(() => setLiveFlash((f) => (f === id ? null : f)), 1500)
        })
        .catch(() => {})
    } else if (kind === 'summary.deleted' && ev.summary_id) {
      setItems((cur) => cur.filter((x) => x.id !== ev.summary_id))
    }
  })

  async function create(e: React.FormEvent) {
    e.preventDefault()
    if (!client || !draft.trim()) return
    setBusy(true)
    try {
      const messages = draft
        .split('\n')
        .map((s) => s.trim())
        .filter(Boolean)
      const s = await client.createSummary(messages)
      setItems((cur) => (cur.some((x) => x.id === s.id) ? cur : [s, ...cur]))
      setDraft('')
    } finally {
      setBusy(false)
    }
  }

  async function act(fn: Promise<unknown>) {
    await fn
    await load({ q, participant, tag, includeArchived })
  }

  // Distinct perspectives present in the loaded set, for the facet dropdown.
  const perspectivesPresent = Array.from(
    new Set(items.map((s) => s.perspective).filter((p): p is string => !!p)),
  ).sort()
  // Apply the client-side facets (kind + perspective) over the loaded set.
  const shown = items.filter(
    (s) =>
      (kind === 'all' || kindOf(s) === kind) &&
      (persp === 'all' || s.perspective === persp),
  )

  return (
    <div className="mx-auto max-w-3xl space-y-6">
      {/* Compose */}
      <form onSubmit={create} className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <label className="text-sm font-medium text-slate-700">New summary</label>
        <textarea
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          rows={3}
          placeholder="Paste messages, one per line…"
          className="mt-1 w-full resize-y rounded-lg border border-slate-300 p-2 text-sm outline-none focus:border-accent"
        />
        <div className="mt-2 flex justify-end">
          <button
            type="submit"
            disabled={busy || !draft.trim()}
            className="rounded-lg bg-accent px-4 py-1.5 text-sm font-medium text-accent-fg disabled:opacity-50"
          >
            {busy ? 'Summarizing…' : 'Summarize'}
          </button>
        </div>
      </form>

      {/* Filters (DSH-004/005/009): text, participant, tag, archived. */}
      <form
        onSubmit={(e) => {
          e.preventDefault()
          void applyFilters()
        }}
        className="rounded-xl bg-white p-3 shadow-sm ring-1 ring-slate-200"
      >
        <div className="flex flex-wrap gap-2">
          <input
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder="Search text, key points…"
            className="min-w-[12rem] flex-1 rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
          />
          <input
            value={participant}
            onChange={(e) => setParticipant(e.target.value)}
            placeholder="Participant"
            className="w-40 rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
          />
          <input
            value={tag}
            onChange={(e) => setTag(e.target.value)}
            placeholder="Tag"
            className="w-32 rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
          />
          <button className="rounded-lg bg-accent px-4 text-sm font-medium text-accent-fg">
            Filter
          </button>
        </div>
        <div className="mt-2 flex items-center gap-3 text-xs text-slate-500">
          <label className="flex items-center gap-1">
            <input
              type="checkbox"
              checked={includeArchived}
              onChange={(e) => {
                setIncludeArchived(e.target.checked)
                void load({ q, participant, tag, includeArchived: e.target.checked })
              }}
            />
            Include archived
          </label>
          {(q || participant || tag || includeArchived) && (
            <button
              type="button"
              onClick={() => {
                setQ('')
                setParticipant('')
                setTag('')
                setIncludeArchived(false)
                void load()
              }}
              className="text-slate-500 underline hover:text-slate-700"
            >
              Clear filters
            </button>
          )}
        </div>
      </form>

      {/* Facets + view toggle (ADR-133 §B): kind (rolling/retrospective/…),
          perspective, and a Calendar view. Client-side over the loaded set. */}
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <select
          value={kind}
          onChange={(e) => setKind(e.target.value)}
          className="rounded-lg border border-slate-300 px-2 py-1"
        >
          <option value="all">All kinds</option>
          <option value="scheduled">Scheduled</option>
          <option value="rolling">Rolling</option>
          <option value="retrospective">Retrospective</option>
          <option value="adhoc">Ad-hoc</option>
        </select>
        <select
          value={persp}
          onChange={(e) => setPersp(e.target.value)}
          disabled={perspectivesPresent.length === 0}
          className="rounded-lg border border-slate-300 px-2 py-1 disabled:opacity-50"
        >
          <option value="all">All perspectives</option>
          {perspectivesPresent.map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
        </select>
        {(() => {
          const regenerable = shown.filter((s) => s.period_end > s.period_start)
          return regenerable.length > 0 ? (
            <button
              type="button"
              onClick={async () => {
                if (!client || !window.confirm(`Regenerate ${regenerable.length} summary(ies)?`)) return
                const r = await client.bulkRegenerate(regenerable.map((s) => s.id))
                setPublishMsg(`Regenerated ${r.regenerated}, skipped ${r.skipped}`)
                setTimeout(() => setPublishMsg(null), 3000)
                await load({ q, participant, tag, includeArchived })
              }}
              className="rounded-lg border border-slate-300 px-2 py-1 text-xs text-slate-600 hover:bg-slate-50"
            >
              ↻ Regenerate {regenerable.length}
            </button>
          ) : null
        })()}
        {publishMsg && <span className="text-xs text-accent">{publishMsg}</span>}
        <div className="ml-auto inline-flex overflow-hidden rounded-lg ring-1 ring-slate-300">
          {(['list', 'calendar'] as const).map((v) => (
            <button
              key={v}
              type="button"
              onClick={() => setView(v)}
              className={`px-3 py-1 capitalize ${view === v ? 'bg-accent text-accent-fg' : 'text-slate-600'}`}
            >
              {v}
            </button>
          ))}
        </div>
      </div>

      {/* List / Calendar */}
      {shown.length === 0 ? (
        <p className="py-12 text-center text-sm text-slate-400">
          {items.length === 0 ? 'No summaries yet.' : 'No summaries match the filters.'}
        </p>
      ) : view === 'calendar' ? (
        <CalendarView
          items={shown}
          onPick={(id) => {
            setExpanded(id)
            setView('list')
          }}
        />
      ) : (
        <ul className="space-y-3">
          {shown.map((s) => (
            <li
              key={s.id}
              className={`rounded-xl bg-white p-4 shadow-sm ring-1 transition ${
                liveFlash === s.id ? 'ring-2 ring-accent' : 'ring-slate-200'
              }`}
            >
              <div className="flex items-start justify-between gap-3">
                <button
                  onClick={() => setExpanded(expanded === s.id ? null : s.id)}
                  className="min-w-0 flex-1 text-left"
                >
                  <p className="truncate font-medium text-slate-800">{s.text || '(no text)'}</p>
                  <p className="mt-0.5 text-xs text-slate-400">
                    {when(s.created_at)} · {s.model}
                    {s.degraded && ' · degraded'}
                    {s.coherence_score != null && (
                      <span
                        className={
                          s.coherence_score < 0.5 ? 'text-red-500' : 'text-slate-400'
                        }
                        title="Coherence: share of claims grounded in source messages"
                      >
                        {' · '}
                        {(s.coherence_score * 100).toFixed(0)}% grounded
                      </span>
                    )}
                    {s.pinned && ' · 📌'}
                  </p>
                  {s.tags.length > 0 && (
                    <div className="mt-1 flex flex-wrap gap-1">
                      {s.tags.map((t) => (
                        <span
                          key={t}
                          className="rounded bg-accent/10 px-1.5 py-0.5 text-xs text-accent"
                        >
                          {t}
                        </span>
                      ))}
                    </div>
                  )}
                </button>
                <div className="flex shrink-0 gap-1">
                  {s.period_end > s.period_start && (
                    <button
                      title="Regenerate over the same window"
                      onClick={() => void act(client!.regenerateSummary(s.id))}
                      className="rounded px-2 py-1 text-sm hover:bg-slate-100"
                    >
                      🔄
                    </button>
                  )}
                  <button
                    title={s.pinned ? 'Unpin' : 'Pin'}
                    onClick={() => void act(client!.setPinned(s.id, !s.pinned))}
                    className="rounded px-2 py-1 text-sm hover:bg-slate-100"
                  >
                    📌
                  </button>
                  <button
                    title="Archive"
                    onClick={() => void act(client!.setArchived(s.id, true))}
                    className="rounded px-2 py-1 text-sm hover:bg-slate-100"
                  >
                    🗄
                  </button>
                  <button
                    title="Delete"
                    onClick={() => void act(client!.deleteSummary(s.id))}
                    className="rounded px-2 py-1 text-sm hover:bg-red-50"
                  >
                    🗑
                  </button>
                </div>
              </div>

              {expanded === s.id && (
                <div className="mt-3 space-y-3 border-t border-slate-100 pt-3 text-sm">
                  {/* Full body, rendered as markdown (one line for a normal summary;
                      a multi-section document for a rolling/weekly digest). */}
                  {s.text.trim() && (
                    <div className="text-slate-700">
                      <Markdown content={s.text} />
                    </div>
                  )}
                  {s.key_points.length > 0 && (
                    <div>
                      <p className="font-medium text-slate-600">Key points</p>
                      <ul className="mt-1 list-disc pl-5 text-slate-700">
                        {s.key_points.map((k, i) => (
                          <li key={i}>
                            {k.text}
                            {k.references.length > 0 && (
                              <span
                                className="ml-1 text-xs text-slate-400"
                                title={k.references
                                  .map((r) => `#${r.position} ${r.author_name}: ${r.snippet}`)
                                  .join('\n')}
                              >
                                (sources:{' '}
                                {k.references.map((r) => `#${r.position} ${r.author_name}`).join(', ')})
                              </span>
                            )}
                          </li>
                        ))}
                      </ul>
                    </div>
                  )}
                  {s.action_items.length > 0 && (
                    <div>
                      <p className="font-medium text-slate-600">Action items</p>
                      <ul className="mt-1 list-disc pl-5 text-slate-700">
                        {s.action_items.map((a, i) => (
                          <li key={i}>
                            {a.text}
                            {a.assignee && <span className="text-slate-400"> — {a.assignee}</span>}
                          </li>
                        ))}
                      </ul>
                    </div>
                  )}
                  {s.participants.length > 0 && (
                    <p className="text-slate-500">
                      <span className="font-medium text-slate-600">Participants: </span>
                      {s.participants.join(', ')}
                    </p>
                  )}

                  {/* Metadata panel (ADR-106): collapsed by default; surfaces the
                      provenance/quality fields stored with the summary. */}
                  <details className="text-xs text-slate-500">
                    <summary className="cursor-pointer font-medium text-slate-600">Metadata</summary>
                    <dl className="mt-1.5 grid grid-cols-[8rem_1fr] gap-x-3 gap-y-1">
                      <dt className="text-slate-400">Model</dt>
                      <dd>
                        {s.model}
                        {s.degraded && <span className="text-amber-600"> · degraded (cheaper model used)</span>}
                      </dd>
                      <dt className="text-slate-400">Scope</dt>
                      <dd>{scopeLabel(s)}</dd>
                      <dt className="text-slate-400">Kind</dt>
                      <dd className="capitalize">{kindOf(s)}</dd>
                      {s.perspective && (
                        <>
                          <dt className="text-slate-400">Perspective</dt>
                          <dd className="capitalize">{s.perspective}</dd>
                        </>
                      )}
                      <dt className="text-slate-400">Cost</dt>
                      <dd>${(s.cost_micros / 1_000_000).toFixed(4)}</dd>
                      {(s.latency_ms > 0 || s.input_tokens > 0 || s.output_tokens > 0) && (
                        <>
                          <dt className="text-slate-400">Latency</dt>
                          <dd>
                            {s.latency_ms === 0
                              ? '<1 ms'
                              : s.latency_ms < 1000
                                ? `${s.latency_ms} ms`
                                : `${(s.latency_ms / 1000).toFixed(1)} s`}
                          </dd>
                        </>
                      )}
                      {(s.input_tokens > 0 || s.output_tokens > 0) && (
                        <>
                          <dt className="text-slate-400">Tokens</dt>
                          <dd>
                            {s.input_tokens.toLocaleString()} in ·{' '}
                            {s.output_tokens.toLocaleString()} out
                          </dd>
                        </>
                      )}
                      {s.coherence_score != null && (
                        <>
                          <dt className="text-slate-400">Grounded</dt>
                          <dd className={s.coherence_score < 0.5 ? 'text-red-500' : ''}>
                            {(s.coherence_score * 100).toFixed(0)}% of claims found in sources
                          </dd>
                        </>
                      )}
                      <dt className="text-slate-400">Created</dt>
                      <dd>{when(s.created_at)}</dd>
                      {s.period_end > s.period_start && (
                        <>
                          <dt className="text-slate-400">Covered</dt>
                          <dd>
                            {new Date(s.period_start * 1000).toLocaleString()} —{' '}
                            {new Date(s.period_end * 1000).toLocaleString()}
                          </dd>
                        </>
                      )}
                      <dt className="text-slate-400">Extracted</dt>
                      <dd>
                        {s.key_points.length} key point{s.key_points.length === 1 ? '' : 's'} ·{' '}
                        {s.action_items.length} action{s.action_items.length === 1 ? '' : 's'} ·{' '}
                        {s.citations.length} source{s.citations.length === 1 ? '' : 's'}
                      </dd>
                      {s.technical_terms.length > 0 && (
                        <>
                          <dt className="text-slate-400">Technical terms</dt>
                          <dd>{s.technical_terms.join(', ')}</dd>
                        </>
                      )}
                      {s.tags.length > 0 && (
                        <>
                          <dt className="text-slate-400">Tags</dt>
                          <dd>{s.tags.join(', ')}</dd>
                        </>
                      )}
                      <dt className="text-slate-400">ID</dt>
                      <dd className="break-all font-mono">{s.id}</dd>
                    </dl>
                  </details>
                  {dests.length > 0 && (
                    <div className="mt-2 flex items-center gap-2 text-xs text-slate-500">
                      <span>Publish to:</span>
                      <select
                        defaultValue=""
                        onChange={(e) => {
                          if (e.target.value) void publishTo(s.id, e.target.value)
                          e.target.value = ''
                        }}
                        className="rounded border border-slate-300 px-1.5 py-1"
                      >
                        <option value="">choose destination…</option>
                        {dests.map((d) => (
                          <option key={d.id} value={d.id}>
                            {d.kind} ({d.id.slice(0, 8)})
                          </option>
                        ))}
                      </select>
                      {publishMsg && <span className="text-accent">{publishMsg}</span>}
                    </div>
                  )}
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

/// Month-grid calendar of summaries by creation day (ADR-133 §B). Shows the
/// month of the most recent summary; each day with summaries lists them as
/// clickable chips that jump to the expanded card in the list view.
function CalendarView({ items, onPick }: { items: Summary[]; onPick: (id: string) => void }) {
  const byDay = new Map<string, Summary[]>()
  for (const s of items) {
    const key = new Date(s.created_at * 1000).toDateString()
    const arr = byDay.get(key) ?? []
    arr.push(s)
    byDay.set(key, arr)
  }
  const latest = new Date(Math.max(...items.map((s) => s.created_at)) * 1000)
  const year = latest.getFullYear()
  const month = latest.getMonth()
  const startDow = new Date(year, month, 1).getDay()
  const daysInMonth = new Date(year, month + 1, 0).getDate()
  const cells: (number | null)[] = []
  for (let i = 0; i < startDow; i++) cells.push(null)
  for (let d = 1; d <= daysInMonth; d++) cells.push(d)

  const monthName = latest.toLocaleString(undefined, { month: 'long', year: 'numeric' })
  return (
    <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
      <p className="mb-2 text-sm font-medium text-slate-700">{monthName}</p>
      <div className="grid grid-cols-7 gap-1 text-xs">
        {['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'].map((d) => (
          <div key={d} className="px-1 py-0.5 text-center font-medium text-slate-400">
            {d}
          </div>
        ))}
        {cells.map((d, i) => {
          if (d == null) return <div key={`b${i}`} />
          const key = new Date(year, month, d).toDateString()
          const day = byDay.get(key) ?? []
          return (
            <div
              key={d}
              className={`min-h-[3.5rem] rounded border p-1 ${
                day.length ? 'border-accent/40 bg-accent/5' : 'border-slate-100'
              }`}
            >
              <div className="text-right text-[10px] text-slate-400">{d}</div>
              {day.slice(0, 3).map((s) => (
                <button
                  key={s.id}
                  onClick={() => onPick(s.id)}
                  title={s.text || s.id}
                  className="mt-0.5 block w-full truncate rounded bg-white px-1 text-left text-[10px] text-slate-600 ring-1 ring-slate-200 hover:ring-accent"
                >
                  {s.text || '(summary)'}
                </button>
              ))}
              {day.length > 3 && <div className="text-[10px] text-slate-400">+{day.length - 3}</div>}
            </div>
          )
        })}
      </div>
    </div>
  )
}
