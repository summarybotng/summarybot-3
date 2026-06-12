import { useEffect, useState } from 'react'
import { useAuth } from '../auth'
import type { KnowledgeHit, WikiPage } from '../types'

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

// Minimal markdown renderer for the synthesized page: headings (#/##/###) and
// bullet lists, everything else as paragraphs. Enough for the LLM's topic-
// grouped output without pulling in a markdown dependency.
function Markdown({ content }: { content: string }) {
  const lines = content.split('\n')
  const blocks: React.ReactNode[] = []
  let bullets: string[] = []

  const flushBullets = () => {
    if (bullets.length === 0) return
    blocks.push(
      <ul key={`ul-${blocks.length}`} className="my-2 list-disc space-y-1 pl-5 text-sm text-slate-700">
        {bullets.map((b, i) => (
          <li key={i}>{inline(b)}</li>
        ))}
      </ul>,
    )
    bullets = []
  }

  for (const raw of lines) {
    const line = raw.trimEnd()
    if (/^#{1}\s+/.test(line)) {
      flushBullets()
      blocks.push(
        <h3 key={blocks.length} className="mt-3 text-lg font-semibold text-slate-800">
          {inline(line.replace(/^#\s+/, ''))}
        </h3>,
      )
    } else if (/^#{2}\s+/.test(line)) {
      flushBullets()
      blocks.push(
        <h4 key={blocks.length} className="mt-3 font-semibold text-slate-700">
          {inline(line.replace(/^#{2}\s+/, ''))}
        </h4>,
      )
    } else if (/^#{3,}\s+/.test(line)) {
      flushBullets()
      blocks.push(
        <h5 key={blocks.length} className="mt-2 text-sm font-semibold text-slate-600">
          {inline(line.replace(/^#{3,}\s+/, ''))}
        </h5>,
      )
    } else if (/^[-*]\s+/.test(line)) {
      bullets.push(line.replace(/^[-*]\s+/, ''))
    } else if (line.trim() === '') {
      flushBullets()
    } else {
      flushBullets()
      blocks.push(
        <p key={blocks.length} className="my-2 text-sm text-slate-700">
          {inline(line)}
        </p>,
      )
    }
  }
  flushBullets()
  return <div>{blocks}</div>
}

// Inline emphasis: **bold** and _italic_ / *italic*. Splits the text on those
// markers and wraps the captured runs; everything else is plain text. Enough
// for the synthesizer's output without a full markdown parser.
function inline(text: string): React.ReactNode {
  const parts: React.ReactNode[] = []
  const re = /\*\*(.+?)\*\*|_(.+?)_|\*(.+?)\*/g
  let last = 0
  let m: RegExpExecArray | null
  let i = 0
  while ((m = re.exec(text)) !== null) {
    if (m.index > last) parts.push(text.slice(last, m.index))
    if (m[1] !== undefined) {
      parts.push(
        <strong key={i++} className="font-semibold text-slate-800">
          {m[1]}
        </strong>,
      )
    } else {
      parts.push(<em key={i++}>{m[2] ?? m[3]}</em>)
    }
    last = re.lastIndex
  }
  if (last < text.length) parts.push(text.slice(last))
  return parts
}
