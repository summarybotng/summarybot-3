import { useCallback, useEffect, useState } from 'react'
import { useAuth } from '../auth'
import type { WorkspaceCoverage } from '../types'

// Coverage view (ADR-133): how much of each channel's stored history is covered
// by summaries — server-wide totals + a per-channel breakdown with gap periods.
// Generalizes the WhatsApp-only coverage to every source. Read-only.

function fmtDate(secs: number | null): string {
  if (!secs) return '—'
  return new Date(secs * 1000).toLocaleDateString()
}

function pctTone(p: number): string {
  if (p >= 80) return 'text-emerald-600'
  if (p >= 40) return 'text-amber-600'
  return 'text-red-500'
}

export function Coverage() {
  const { client } = useAuth()
  const [cov, setCov] = useState<WorkspaceCoverage | null>(null)
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)

  const load = useCallback(async () => {
    if (!client) return
    setBusy(true)
    setErr(null)
    try {
      setCov(await client.workspaceCoverage())
    } catch {
      setErr('Could not load coverage.')
    } finally {
      setBusy(false)
    }
  }, [client])

  useEffect(() => {
    void load()
  }, [load])

  const stat = (label: string, value: string) => (
    <div className="rounded-lg bg-slate-50 px-3 py-2 ring-1 ring-slate-200">
      <div className="text-xs text-slate-400">{label}</div>
      <div className="text-lg font-semibold text-slate-800">{value}</div>
    </div>
  )

  return (
    <div className="mx-auto max-w-3xl space-y-4">
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <div className="flex items-start justify-between gap-3">
          <div>
            <h2 className="font-semibold text-slate-800">Content Coverage</h2>
            <p className="mt-1 text-sm text-slate-500">
              How much of each channel's stored history has been summarized, with the gap
              periods that have no summary.
            </p>
          </div>
          <button
            onClick={load}
            disabled={busy}
            className="shrink-0 rounded-lg border border-slate-300 px-3 py-1.5 text-sm disabled:opacity-50"
          >
            {busy ? 'Loading…' : 'Refresh'}
          </button>
        </div>

        {err && <p className="mt-3 text-sm text-red-600">{err}</p>}

        {cov && (
          <>
            <div className="mt-4 grid grid-cols-2 gap-2 sm:grid-cols-4">
              {stat('Coverage', `${cov.total_coverage_percent.toFixed(1)}%`)}
              {stat('Gaps', String(cov.total_gaps))}
              {stat('Channels', `${cov.covered_channels}/${cov.total_channels}`)}
              {stat('Summaries', String(cov.total_summaries))}
            </div>
            <p className="mt-2 text-xs text-slate-400">
              Content range: {fmtDate(cov.earliest_content)} — {fmtDate(cov.latest_content)}.
              Coverage can exceed 100% when summary windows overlap (e.g. rolling digests).
            </p>
          </>
        )}
      </div>

      {cov && (
        <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
          <h3 className="text-sm font-medium text-slate-600">Channel coverage</h3>
          {cov.channels.length === 0 && (
            <p className="mt-3 text-sm text-slate-400">
              No channels with stored messages yet — sync a source or import a chat.
            </p>
          )}
          <ul className="mt-2 divide-y divide-slate-100">
            {cov.channels.map((c) => (
              <li key={c.channel_id} className="py-3 text-sm">
                <div className="flex items-center justify-between gap-3">
                  <span className="min-w-0 truncate font-mono text-slate-700">{c.channel_id}</span>
                  <span className={`shrink-0 font-semibold ${pctTone(c.coverage_percent)}`}>
                    {c.coverage_percent.toFixed(0)}%
                  </span>
                </div>
                <div className="mt-0.5 text-xs text-slate-400">
                  {c.message_count} message{c.message_count === 1 ? '' : 's'} ·{' '}
                  {c.summary_count} summar{c.summary_count === 1 ? 'y' : 'ies'} ·{' '}
                  {c.gap_count} gap{c.gap_count === 1 ? '' : 's'} · {fmtDate(c.earliest_content)} —{' '}
                  {fmtDate(c.latest_content)}
                </div>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  )
}
