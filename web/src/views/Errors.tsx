import { useCallback, useEffect, useState } from 'react'
import { useAuth } from '../auth'
import type { Errors as ErrorsData } from '../types'

// Errors view (ADR-133 A3; ADR-031): operational failures (sync/summarize/
// deliver) with operation, severity, scope — resolvable. Read + resolve only.

const SEV_TONE: Record<string, string> = {
  error: 'bg-red-100 text-red-700',
  warning: 'bg-amber-100 text-amber-800',
}

export function Errors() {
  const { client } = useAuth()
  const [data, setData] = useState<ErrorsData | null>(null)
  const [busy, setBusy] = useState(false)
  const [showResolved, setShowResolved] = useState(false)

  const load = useCallback(
    async (incl: boolean) => {
      if (!client) return
      setBusy(true)
      try {
        setData(await client.listErrors(incl))
      } catch {
        setData({ unresolved: 0, errors: [] })
      } finally {
        setBusy(false)
      }
    },
    [client],
  )

  useEffect(() => {
    void load(showResolved)
  }, [load, showResolved])

  async function resolveOne(id: string) {
    if (!client) return
    await client.resolveError(id)
    await load(showResolved)
  }

  async function resolveAll() {
    if (!client) return
    await client.resolveAllErrors()
    await load(showResolved)
  }

  return (
    <div className="mx-auto max-w-3xl space-y-4">
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <div className="flex items-start justify-between gap-3">
          <div>
            <h2 className="font-semibold text-slate-800">
              Errors
              {data && data.unresolved > 0 && (
                <span className="ml-2 rounded-full bg-red-100 px-2 py-0.5 text-xs font-medium text-red-700">
                  {data.unresolved} unresolved
                </span>
              )}
            </h2>
            <p className="mt-1 text-sm text-slate-500">
              Operational failures — a sync that couldn't read a channel, a failed summary or
              delivery — with the operation and scope. Resolve them once handled.
            </p>
          </div>
          <div className="flex shrink-0 gap-2">
            <button
              onClick={resolveAll}
              disabled={busy || !data || data.unresolved === 0}
              className="rounded-lg border border-slate-300 px-3 py-1.5 text-sm disabled:opacity-50"
            >
              Bulk resolve
            </button>
            <button
              onClick={() => void load(showResolved)}
              disabled={busy}
              className="rounded-lg border border-slate-300 px-3 py-1.5 text-sm disabled:opacity-50"
            >
              {busy ? 'Loading…' : 'Refresh'}
            </button>
          </div>
        </div>
        <label className="mt-3 flex items-center gap-2 text-sm text-slate-600">
          <input
            type="checkbox"
            checked={showResolved}
            onChange={(e) => setShowResolved(e.target.checked)}
          />
          Show resolved
        </label>
      </div>

      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        {data?.errors.length === 0 && (
          <p className="py-3 text-sm text-slate-400">
            No errors{showResolved ? '' : ' to resolve'} — operations are healthy. 🎉
          </p>
        )}
        <ul className="divide-y divide-slate-100">
          {data?.errors.map((e) => (
            <li key={e.id} className="flex items-start justify-between gap-3 py-3 text-sm">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <span
                    className={`rounded px-1.5 py-0.5 text-xs font-medium ${
                      SEV_TONE[e.severity] ?? 'bg-slate-100 text-slate-600'
                    }`}
                  >
                    {e.severity}
                  </span>
                  <span className="font-medium text-slate-800">{e.operation}</span>
                  <span className="text-xs text-slate-400">{e.error_class}</span>
                  {e.channel_id && (
                    <span className="font-mono text-xs text-slate-400">#{e.channel_id}</span>
                  )}
                </div>
                <p className="mt-1 break-words text-slate-600">{e.message}</p>
                <div className="mt-0.5 text-xs text-slate-400">
                  {new Date(e.created_at * 1000).toLocaleString()}
                </div>
              </div>
              {!e.resolved ? (
                <button
                  onClick={() => void resolveOne(e.id)}
                  className="shrink-0 rounded border border-slate-300 px-2 py-0.5 text-xs"
                >
                  Resolve
                </button>
              ) : (
                <span className="shrink-0 text-xs text-emerald-600">resolved</span>
              )}
            </li>
          ))}
        </ul>
      </div>
    </div>
  )
}
