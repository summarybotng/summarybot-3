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

      {/* List */}
      {items.length === 0 ? (
        <p className="py-12 text-center text-sm text-slate-400">No summaries yet.</p>
      ) : (
        <ul className="space-y-3">
          {items.map((s) => (
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
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
