import { useCallback, useEffect, useState } from 'react'
import { useAuth } from '../auth'
import type { Vectors } from '../types'

// RuVector Explorer (ADR-133 A7): a read-only browser over the knowledge vector
// store — embedding coverage, per-model breakdown, and per-unit metadata. Raw
// vectors aren't shown (use the RVF export for those).

export function RuVector() {
  const { client } = useAuth()
  const [data, setData] = useState<Vectors | null>(null)
  const [err, setErr] = useState<string | null>(null)

  const load = useCallback(async () => {
    if (!client) return
    setErr(null)
    try {
      setData(await client.vectors())
    } catch {
      setErr('Could not load the vector store.')
    }
  }, [client])

  useEffect(() => {
    void load()
  }, [load])

  return (
    <div className="mx-auto max-w-3xl space-y-4">
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <h2 className="font-semibold text-slate-800">Vector Store</h2>
        <p className="mt-1 text-sm text-slate-500">
          The knowledge units backing semantic search, with embedding coverage and model details.
        </p>
        {err && <p className="mt-2 text-sm text-red-600">{err}</p>}
        {data && (
          <>
            <div className="mt-3 grid grid-cols-2 gap-2 sm:grid-cols-3">
              <Stat label="Units" value={String(data.total)} />
              <Stat
                label="Embedded"
                value={`${data.embedded}/${data.total}`}
                tone={data.embedded < data.total ? 'text-amber-600' : 'text-emerald-600'}
              />
              <Stat label="Models" value={String(data.models.length)} />
            </div>
            {data.models.length > 0 && (
              <div className="mt-2 flex flex-wrap gap-1.5 text-xs">
                {data.models.map((m) => (
                  <span key={m.model} className="rounded bg-slate-100 px-2 py-1 text-slate-600">
                    {m.model} · {m.count} · {m.dims}d
                  </span>
                ))}
              </div>
            )}
          </>
        )}
      </div>

      {data && (
        <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
          <h3 className="text-sm font-medium text-slate-600">Units</h3>
          {data.units.length === 0 && (
            <p className="mt-3 text-sm text-slate-400">No knowledge units yet.</p>
          )}
          <ul className="mt-2 divide-y divide-slate-100">
            {data.units.map((u) => (
              <li key={u.id} className="py-2 text-sm">
                <div className="flex items-center gap-2">
                  <span className="rounded bg-slate-100 px-1.5 py-0.5 text-xs text-slate-500">
                    {u.kind}
                  </span>
                  {u.has_embedding ? (
                    <span className="text-xs text-emerald-600">{u.dims}d · {u.model}</span>
                  ) : (
                    <span className="text-xs text-amber-600">unembedded</span>
                  )}
                  {u.source_channel && (
                    <span className="font-mono text-xs text-slate-400">#{u.source_channel}</span>
                  )}
                </div>
                <p className="mt-0.5 truncate text-slate-700">{u.text}</p>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  )
}

function Stat({ label, value, tone }: { label: string; value: string; tone?: string }) {
  return (
    <div className="rounded-lg bg-slate-50 px-3 py-2 ring-1 ring-slate-200">
      <div className="text-xs text-slate-400">{label}</div>
      <div className={`text-lg font-semibold ${tone ?? 'text-slate-800'}`}>{value}</div>
    </div>
  )
}
