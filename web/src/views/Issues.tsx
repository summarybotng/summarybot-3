import { useCallback, useEffect, useState } from 'react'
import { useAuth } from '../auth'
import type { Issues as IssuesData } from '../types'

// Issues view (ADR-039): the triage queue for user-submitted problem reports —
// distinct from the system Errors log. Admins review + resolve. Filterable by
// severity and paginated (ADR-133 D4).

const STATUS_TONE: Record<string, string> = {
  open: 'bg-red-100 text-red-700',
  investigating: 'bg-amber-100 text-amber-800',
  resolved: 'bg-emerald-100 text-emerald-800',
  wont_fix: 'bg-slate-100 text-slate-500',
}
const SEVERITY_TONE: Record<string, string> = {
  low: 'bg-slate-100 text-slate-600',
  medium: 'bg-sky-100 text-sky-700',
  high: 'bg-orange-100 text-orange-800',
  critical: 'bg-red-100 text-red-700',
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

const PAGE_SIZE = 25

const EMPTY: IssuesData = { open: 0, total: 0, categories: [], severities: [], issues: [] }

export function Issues() {
  const { client } = useAuth()
  const [data, setData] = useState<IssuesData | null>(null)
  const [showResolved, setShowResolved] = useState(false)
  const [severity, setSeverity] = useState('')
  const [page, setPage] = useState(0)

  const load = useCallback(async () => {
    if (!client) return
    try {
      setData(
        await client.listIssues({
          includeResolved: showResolved,
          severity: severity || undefined,
          limit: PAGE_SIZE,
          offset: page * PAGE_SIZE,
        }),
      )
    } catch {
      setData(EMPTY)
    }
  }, [client, showResolved, severity, page])

  useEffect(() => {
    void load()
  }, [load])

  // Reset to the first page whenever a filter changes.
  useEffect(() => {
    setPage(0)
  }, [showResolved, severity])

  async function setStatus(id: string, status: string) {
    if (!client) return
    await client.setIssueStatus(id, status)
    await load()
  }

  const total = data?.total ?? 0
  const pageCount = Math.max(1, Math.ceil(total / PAGE_SIZE))
  const severities = data?.severities ?? ['low', 'medium', 'high', 'critical']

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
        <div className="mt-2 flex flex-wrap items-center gap-4">
          <label className="flex items-center gap-2 text-sm text-slate-600">
            <input
              type="checkbox"
              checked={showResolved}
              onChange={(e) => setShowResolved(e.target.checked)}
            />
            Show resolved
          </label>
          <label className="flex items-center gap-2 text-sm text-slate-600">
            Severity
            <select
              value={severity}
              onChange={(e) => setSeverity(e.target.value)}
              className="rounded-lg border border-slate-300 px-2 py-1 text-sm"
            >
              <option value="">All</option>
              {severities.map((s) => (
                <option key={s} value={s}>
                  {s}
                </option>
              ))}
            </select>
          </label>
        </div>
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
                  <div className="flex flex-wrap items-center gap-2">
                    <span
                      className={`rounded px-1.5 py-0.5 text-xs font-medium ${STATUS_TONE[i.status] ?? 'bg-slate-100'}`}
                    >
                      {i.status}
                    </span>
                    <span
                      className={`rounded px-1.5 py-0.5 text-xs font-medium ${SEVERITY_TONE[i.severity] ?? 'bg-slate-100'}`}
                    >
                      {i.severity}
                    </span>
                    <span className="font-medium text-slate-800">{i.category}</span>
                    {i.page_url && (
                      <span className="font-mono text-xs text-slate-400" title={i.page_url}>
                        {i.page_url}
                      </span>
                    )}
                  </div>
                  <p className="mt-1 whitespace-pre-wrap text-slate-700">{i.description}</p>
                  <div className="mt-0.5 text-xs text-slate-400">
                    {i.reported_by ? `by ${i.reported_by} · ` : ''}
                    {new Date(i.created_at * 1000).toLocaleString()}
                    {i.browser && (
                      <span className="ml-1 text-slate-300" title={i.browser}>
                        · {i.browser.slice(0, 40)}
                        {i.browser.length > 40 ? '…' : ''}
                      </span>
                    )}
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

        {total > PAGE_SIZE && (
          <div className="mt-3 flex items-center justify-between border-t border-slate-100 pt-3 text-sm text-slate-500">
            <span>
              {page * PAGE_SIZE + 1}–{Math.min((page + 1) * PAGE_SIZE, total)} of {total}
            </span>
            <div className="flex gap-2">
              <button
                onClick={() => setPage((p) => Math.max(0, p - 1))}
                disabled={page === 0}
                className="rounded border border-slate-300 px-2 py-0.5 disabled:opacity-40"
              >
                ‹ Prev
              </button>
              <button
                onClick={() => setPage((p) => Math.min(pageCount - 1, p + 1))}
                disabled={page >= pageCount - 1}
                className="rounded border border-slate-300 px-2 py-0.5 disabled:opacity-40"
              >
                Next ›
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}
