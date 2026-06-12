import { useState } from 'react'
import { useAuth } from '../auth'
import type { KnowledgeHit } from '../types'

// Knowledge search (ADR-127). Semantic search over the units extracted from this
// workspace's summaries; each hit links back to the summary and its source
// messages (provenance, COH-005).
export function Knowledge() {
  const { client } = useAuth()
  const [q, setQ] = useState('')
  const [hits, setHits] = useState<KnowledgeHit[] | null>(null)
  const [busy, setBusy] = useState(false)

  async function search(e: React.FormEvent) {
    e.preventDefault()
    if (!client || !q.trim()) return
    setBusy(true)
    try {
      setHits(await client.searchKnowledge(q.trim(), 15))
    } catch {
      setHits([])
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="mx-auto max-w-2xl space-y-4">
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <h2 className="font-semibold text-slate-800">Knowledge search</h2>
        <p className="mt-1 text-sm text-slate-500">
          Semantic search across everything summarized in this workspace. Matches are knowledge
          units (key points, decisions, action items) extracted from summaries.
        </p>
        <form onSubmit={search} className="mt-4 flex gap-2">
          <input
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder="e.g. database migration decisions"
            className="flex-1 rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
          />
          <button
            type="submit"
            disabled={busy || !q.trim()}
            className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-fg disabled:opacity-50"
          >
            {busy ? 'Searching…' : 'Search'}
          </button>
        </form>
      </div>

      {hits !== null && (
        <div className="space-y-2">
          {hits.length === 0 && (
            <p className="rounded-lg bg-white p-4 text-sm text-slate-400 ring-1 ring-slate-200">
              No matches — summarize some conversations first, then search.
            </p>
          )}
          {hits.map((h) => (
            <div key={h.id} className="rounded-lg bg-white p-3 text-sm ring-1 ring-slate-200">
              <div className="flex items-start justify-between gap-3">
                <span className="text-slate-800">{h.text}</span>
                <span className="shrink-0 rounded bg-accent/10 px-1.5 py-0.5 text-xs text-accent">
                  {(h.score * 100).toFixed(0)}%
                </span>
              </div>
              <div className="mt-1 text-xs text-slate-400">
                <span className="rounded bg-slate-100 px-1.5 py-0.5">{h.kind}</span>
                {h.source_ids.length > 0 && (
                  <span className="ml-2">{h.source_ids.length} source message(s)</span>
                )}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}
