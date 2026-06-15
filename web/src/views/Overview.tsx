import { useCallback, useEffect, useState } from 'react'
import { useAuth } from '../auth'
import type { Overview as OverviewData } from '../types'

// Overview (ADR-133 A6): the workspace dashboard home — headline counts, recent
// summaries, coverage, and config status. Read-only.

export function Overview({ onNavigate }: { onNavigate?: (tab: string) => void }) {
  const { client } = useAuth()
  const [data, setData] = useState<OverviewData | null>(null)
  const [err, setErr] = useState<string | null>(null)

  const load = useCallback(async () => {
    if (!client) return
    setErr(null)
    try {
      setData(await client.overview())
    } catch {
      setErr('Could not load the overview.')
    }
  }, [client])

  useEffect(() => {
    void load()
  }, [load])

  const card = (label: string, value: string, tab?: string, tone?: string) => (
    <button
      onClick={() => tab && onNavigate?.(tab)}
      disabled={!tab}
      className={`rounded-xl bg-white p-4 text-left shadow-sm ring-1 ring-slate-200 ${
        tab ? 'hover:ring-accent' : ''
      }`}
    >
      <div className="text-xs uppercase tracking-wide text-slate-400">{label}</div>
      <div className={`mt-1 text-2xl font-semibold ${tone ?? 'text-slate-800'}`}>{value}</div>
    </button>
  )

  return (
    <div className="mx-auto max-w-3xl space-y-4">
      <div>
        <h2 className="font-semibold text-slate-800">Overview</h2>
        <p className="mt-1 text-sm text-slate-500">Your workspace at a glance.</p>
        {err && <p className="mt-2 text-sm text-red-600">{err}</p>}
      </div>

      {data && (
        <>
          <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
            {card('Summaries', String(data.summary_count), 'summaries')}
            {card('Schedules', String(data.schedule_count), 'schedules')}
            {card('Coverage', `${data.coverage_percent.toFixed(0)}%`, 'coverage')}
            {card('Members', String(data.member_count), 'members')}
            {card('Spend', `$${(data.total_cost_micros / 1_000_000).toFixed(2)}`, 'spend')}
            {card(
              'Open errors',
              String(data.unresolved_errors),
              'errors',
              data.unresolved_errors > 0 ? 'text-red-600' : 'text-slate-800',
            )}
          </div>

          <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
            <div className="flex items-center justify-between">
              <h3 className="text-sm font-medium text-slate-600">Recent summaries</h3>
              <button onClick={() => onNavigate?.('summaries')} className="text-xs text-accent">
                View all →
              </button>
            </div>
            {data.recent.length === 0 ? (
              <p className="mt-3 text-sm text-slate-400">No summaries yet.</p>
            ) : (
              <ul className="mt-2 divide-y divide-slate-100">
                {data.recent.map((r) => (
                  <li key={r.id} className="py-2 text-sm">
                    <div className="truncate text-slate-700">{r.title || '(no text)'}</div>
                    <div className="text-xs text-slate-400">
                      {new Date(r.created_at * 1000).toLocaleString()}
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </>
      )}
    </div>
  )
}
