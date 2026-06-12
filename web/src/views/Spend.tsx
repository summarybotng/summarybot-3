import { useEffect, useState } from 'react'
import { useAuth } from '../auth'
import type { Spend as SpendData } from '../types'

// Cost analytics (ADR-125). Summarization spend rolled up from this workspace's
// stored summaries — total, a recent window, and a per-model breakdown. Money is
// micro-dollars server-side (1 USD = 1_000_000 µ$); shown here as dollars.
function usd(micros: number): string {
  return `$${(micros / 1_000_000).toFixed(micros === 0 ? 0 : 4)}`
}

export function Spend() {
  const { client } = useAuth()
  const [days, setDays] = useState(30)
  const [data, setData] = useState<SpendData | null>(null)
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    if (!client) return
    setLoading(true)
    client
      .spend(days)
      .then(setData)
      .catch(() => setData(null))
      .finally(() => setLoading(false))
  }, [client, days])

  const maxModel = data && data.by_model.length > 0 ? data.by_model[0].cost_micros : 0

  return (
    <div className="mx-auto max-w-2xl space-y-4">
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <div className="flex items-start justify-between gap-3">
          <div>
            <h2 className="font-semibold text-slate-800">Summarization spend</h2>
            <p className="mt-1 text-sm text-slate-500">
              What this workspace has spent on LLM summarization, totaled from stored summaries.
            </p>
          </div>
          <select
            value={days}
            onChange={(e) => setDays(Number(e.target.value))}
            className="shrink-0 rounded-lg border border-slate-300 px-2 py-1.5 text-sm"
          >
            <option value={7}>last 7 days</option>
            <option value={30}>last 30 days</option>
            <option value={90}>last 90 days</option>
          </select>
        </div>

        {loading ? (
          <p className="mt-4 text-sm text-slate-400">Loading…</p>
        ) : !data ? (
          <p className="mt-4 text-sm text-slate-400">No spend data.</p>
        ) : (
          <>
            <div className="mt-4 grid grid-cols-3 gap-3">
              <Stat label="Total" value={usd(data.total_micros)} />
              <Stat label={`Last ${data.recent_days}d`} value={usd(data.recent_micros)} />
              <Stat label="Summaries" value={String(data.summary_count)} />
            </div>

            {data.by_model.length > 0 && (
              <div className="mt-5">
                <p className="text-xs font-medium uppercase tracking-wide text-slate-400">
                  By model
                </p>
                <div className="mt-2 space-y-2">
                  {data.by_model.map((m) => (
                    <div key={m.model}>
                      <div className="flex items-baseline justify-between text-sm">
                        <span className="text-slate-700">{m.model}</span>
                        <span className="text-slate-500">
                          {usd(m.cost_micros)} · {m.count} run{m.count === 1 ? '' : 's'}
                        </span>
                      </div>
                      <div className="mt-1 h-1.5 rounded-full bg-slate-100">
                        <div
                          className="h-1.5 rounded-full bg-accent"
                          style={{
                            width: `${maxModel > 0 ? (m.cost_micros / maxModel) * 100 : 0}%`,
                          }}
                        />
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            )}

            {data.summary_count === 0 && (
              <p className="mt-4 text-sm text-slate-400">
                No summaries yet — spend appears here once you summarize.
              </p>
            )}
          </>
        )}
      </div>
    </div>
  )
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-lg bg-slate-50 p-3">
      <p className="text-xs text-slate-400">{label}</p>
      <p className="mt-0.5 text-lg font-semibold text-slate-800">{value}</p>
    </div>
  )
}
