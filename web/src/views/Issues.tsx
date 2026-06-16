import { useCallback, useEffect, useState } from 'react'
import { useAuth } from '../auth'
import type { Issues as IssuesData } from '../types'

// Issues view (ADR-039): the triage queue for user-submitted problem reports —
// distinct from the system Errors log. Admins review + resolve.

const STATUS_TONE: Record<string, string> = {
  open: 'bg-red-100 text-red-700',
  investigating: 'bg-amber-100 text-amber-800',
  resolved: 'bg-emerald-100 text-emerald-800',
  wont_fix: 'bg-slate-100 text-slate-500',
}
const NEXT: Record<string, { label: string; status: string }[]> = {
  open: [
    { label: 'Investigating', status: 'investigating' },
    { label: 'Resolve', status: 'resolved' },
    { label: "Won't fix", status: 'wont_fix' },
  ],
  investigating: [
    { label: 'Resolve', status: 'resolved' },
    { label: "Won't fix", status: 'wont_fix' },
  ],
  resolved: [{ label: 'Reopen', status: 'open' }],
  wont_fix: [{ label: 'Reopen', status: 'open' }],
}

export function Issues() {
  const { client } = useAuth()
  const [data, setData] = useState<IssuesData | null>(null)
  const [showResolved, setShowResolved] = useState(false)

  const load = useCallback(
    async (incl: boolean) => {
      if (!client) return
      try {
        setData(await client.listIssues(incl))
      } catch {
        setData({ open: 0, categories: [], issues: [] })
      }
    },
    [client],
  )

  useEffect(() => {
    void load(showResolved)
  }, [load, showResolved])

  async function setStatus(id: string, status: string) {
    if (!client) return
    await client.setIssueStatus(id, status)
    await load(showResolved)
  }

  return (
    <div className="mx-auto max-w-3xl space-y-4">
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <h2 className="font-semibold text-slate-800">
          Issues
          {data && data.open > 0 && (
            <span className="ml-2 rounded-full bg-red-100 px-2 py-0.5 text-xs font-medium text-red-700">
              {data.open} open
            </span>
          )}
        </h2>
        <p className="mt-1 text-sm text-slate-500">
          Problems reported by members (the “Report a problem” button). Triage and resolve them
          here — distinct from the system Errors log.
        </p>
        <label className="mt-2 flex items-center gap-2 text-sm text-slate-600">
          <input type="checkbox" checked={showResolved} onChange={(e) => setShowResolved(e.target.checked)} />
          Show resolved
        </label>
      </div>

      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        {data?.issues.length === 0 && (
          <p className="py-3 text-sm text-slate-400">No reports{showResolved ? '' : ' open'}. 🎉</p>
        )}
        <ul className="divide-y divide-slate-100">
          {data?.issues.map((i) => (
            <li key={i.id} className="py-3 text-sm">
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <div className="flex items-center gap-2">
                    <span
                      className={`rounded px-1.5 py-0.5 text-xs font-medium ${STATUS_TONE[i.status] ?? 'bg-slate-100'}`}
                    >
                      {i.status}
                    </span>
                    <span className="font-medium text-slate-800">{i.category}</span>
                    {i.page_url && <span className="font-mono text-xs text-slate-400">{i.page_url}</span>}
                  </div>
                  <p className="mt-1 whitespace-pre-wrap text-slate-700">{i.description}</p>
                  <div className="mt-0.5 text-xs text-slate-400">
                    {i.reported_by ? `by ${i.reported_by} · ` : ''}
                    {new Date(i.created_at * 1000).toLocaleString()}
                  </div>
                </div>
                <div className="flex shrink-0 flex-col gap-1">
                  {(NEXT[i.status] ?? []).map((a) => (
                    <button
                      key={a.status}
                      onClick={() => void setStatus(i.id, a.status)}
                      className="rounded border border-slate-300 px-2 py-0.5 text-xs"
                    >
                      {a.label}
                    </button>
                  ))}
                </div>
              </div>
            </li>
          ))}
        </ul>
      </div>
    </div>
  )
}
