import { useEffect, useState } from 'react'
import { useAuth } from '../auth'
import { Markdown } from '../components/Markdown'
import type { CurationReport, KnowledgeHit, KnowledgeUnit, WikiPage } from '../types'

// Knowledge subsystem (ADR-127). Two parts: a synthesized **Knowledge Base**
// page (WIK-001..003 — the workspace's units organized by topic, regenerable on
// demand) and **semantic search** (KNO-005) over the individual units, each hit
// linking back to its source messages (provenance, COH-005).
export function Knowledge() {
  const { client } = useAuth()
  const [q, setQ] = useState('')
  const [hits, setHits] = useState<KnowledgeHit[] | null>(null)
  const [busy, setBusy] = useState(false)
  const [page, setPage] = useState<WikiPage | null>(null)
  const [synthBusy, setSynthBusy] = useState(false)
  const [synthError, setSynthError] = useState<string | null>(null)
  const [report, setReport] = useState<CurationReport | null>(null)
  const [curateBusy, setCurateBusy] = useState(false)
  const [units, setUnits] = useState<KnowledgeUnit[] | null>(null)
  const [unitsBusy, setUnitsBusy] = useState(false)

  useEffect(() => {
    if (!client) return
    client
      .listWikiPages()
      .then((pages) => setPage(pages[0] ?? null))
      .catch(() => setPage(null))
  }, [client])

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

  async function regenerate() {
    if (!client) return
    setSynthBusy(true)
    setSynthError(null)
    try {
      setPage(await client.synthesizeWiki())
    } catch (err) {
      setSynthError(err instanceof Error ? err.message : 'Synthesis failed')
    } finally {
      setSynthBusy(false)
    }
  }

  async function curate() {
    if (!client) return
    setCurateBusy(true)
    try {
      setReport(await client.curateWiki())
    } catch {
      setReport(null)
    } finally {
      setCurateBusy(false)
    }
  }

  const [pruneBusy, setPruneBusy] = useState(false)
  const [pruneNote, setPruneNote] = useState<string | null>(null)
  async function prune() {
    if (!client) return
    setPruneBusy(true)
    setPruneNote(null)
    try {
      const r = await client.pruneWiki()
      setPruneNote(`Pruned ${r.pruned} duplicate unit(s) — provenance kept on the survivor.`)
      setReport(await client.curateWiki())
      if (units) setUnits(await client.listUnits())
    } catch {
      setPruneNote('Prune failed.')
    } finally {
      setPruneBusy(false)
    }
  }

  async function loadUnits() {
    if (!client) return
    setUnitsBusy(true)
    try {
      setUnits(await client.listUnits())
    } catch {
      setUnits([])
    } finally {
      setUnitsBusy(false)
    }
  }

  return (
    <div className="mx-auto max-w-2xl space-y-4">
      {/* Knowledge Base (WIK-001..003) */}
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <div className="flex items-start justify-between gap-3">
          <div>
            <h2 className="font-semibold text-slate-800">Knowledge base</h2>
            <p className="mt-1 text-sm text-slate-500">
              The facts from this workspace's summaries, organized by topic. Regenerate to fold in
              everything summarized since the last synthesis.
            </p>
          </div>
          <button
            onClick={regenerate}
            disabled={synthBusy}
            className="shrink-0 rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-fg disabled:opacity-50"
          >
            {synthBusy ? 'Synthesizing…' : page ? 'Regenerate' : 'Generate'}
          </button>
        </div>
        {synthError && <p className="mt-3 text-sm text-red-600">{synthError}</p>}
        {page ? (
          <div className="mt-4 border-t border-slate-100 pt-4">
            <Markdown content={page.content_md} />
            <p className="mt-4 text-xs text-slate-400">
              {page.unit_count} knowledge unit(s) · updated{' '}
              {new Date(page.updated_at * 1000).toLocaleString()}
            </p>
          </div>
        ) : (
          !synthBusy && (
            <p className="mt-3 text-sm text-slate-400">
              No knowledge base yet — summarize some conversations, then generate.
            </p>
          )
        )}
      </div>

      {/* AI wiki curator (CUR-*) — advisory health report */}
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <div className="flex items-start justify-between gap-3">
          <div>
            <h2 className="font-semibold text-slate-800">Curator</h2>
            <p className="mt-1 text-sm text-slate-500">
              Review the knowledge base for near-duplicate facts and stale units. Advisory only —
              nothing is changed.
            </p>
          </div>
          <button
            onClick={curate}
            disabled={curateBusy}
            className="shrink-0 rounded-lg border border-accent px-4 py-2 text-sm font-medium text-accent disabled:opacity-50"
          >
            {curateBusy ? 'Reviewing…' : 'Curate'}
          </button>
        </div>
        {report && (
          <div className="mt-4 border-t border-slate-100 pt-4 text-sm">
            <p className="text-slate-600">
              {report.total_units} unit(s) · <span className="font-medium">{report.redundant_count}</span>{' '}
              redundant · <span className="font-medium">{report.stale.length}</span> stale
            </p>
            {report.duplicate_clusters.length > 0 && (
              <div className="mt-3">
                <p className="text-xs font-medium text-slate-600">Duplicate clusters</p>
                <ul className="mt-1 space-y-1">
                  {report.duplicate_clusters.map((c) => (
                    <li key={c.canonical_id} className="rounded-lg bg-amber-50 p-2 text-xs text-slate-600">
                      <span className="rounded bg-slate-100 px-1.5 py-0.5">{c.kind}</span>{' '}
                      <span className="text-slate-800">{c.text}</span>
                      <span className="ml-1 text-amber-700">
                        + {c.duplicate_ids.length} near-duplicate(s)
                      </span>
                    </li>
                  ))}
                </ul>
                <button
                  onClick={prune}
                  disabled={pruneBusy}
                  className="mt-2 rounded-lg bg-accent px-3 py-1.5 text-xs font-medium text-accent-fg disabled:opacity-50"
                >
                  {pruneBusy ? 'Pruning…' : `Prune ${report.redundant_count} duplicate(s)`}
                </button>
                <p className="mt-1 text-xs text-slate-400">
                  Keeps the oldest in each cluster and folds the others' sources into it.
                </p>
              </div>
            )}
            {pruneNote && <p className="mt-2 text-xs text-emerald-700">{pruneNote}</p>}
            {report.duplicate_clusters.length === 0 && report.stale.length === 0 && (
              <p className="mt-2 text-emerald-700">No duplicates or stale units — the knowledge base is healthy.</p>
            )}
          </div>
        )}
      </div>

      {/* Raw knowledge units with provenance (ADR-063) */}
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <div className="flex items-start justify-between gap-3">
          <div>
            <h2 className="font-semibold text-slate-800">Knowledge units (raw)</h2>
            <p className="mt-1 text-sm text-slate-500">
              The individual facts behind the synthesized page, each with the source messages it
              was grounded in.
            </p>
          </div>
          <button
            onClick={loadUnits}
            disabled={unitsBusy}
            className="shrink-0 rounded-lg border border-accent px-4 py-2 text-sm font-medium text-accent disabled:opacity-50"
          >
            {unitsBusy ? 'Loading…' : units ? 'Refresh' : 'Show units'}
          </button>
        </div>
        {units && (
          <div className="mt-4 border-t border-slate-100 pt-3">
            {units.length === 0 ? (
              <p className="text-sm text-slate-400">No knowledge units yet — summarize first.</p>
            ) : (
              <ul className="space-y-1.5">
                {units.map((u) => (
                  <li key={u.id} className="text-sm">
                    <span className="rounded bg-slate-100 px-1.5 py-0.5 text-xs text-slate-600">{u.kind}</span>{' '}
                    <span className="text-slate-800">{u.text}</span>
                    <span className="ml-1 text-xs text-slate-400">
                      · {u.source_ids.length} source{u.source_ids.length === 1 ? '' : 's'}
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}
      </div>

      {/* Semantic search (KNO-005) */}
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

