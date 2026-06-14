import { useCallback, useEffect, useState } from 'react'
import { useAuth } from '../auth'
import type { Job } from '../types'

// Jobs view (ADR-040): long-running / background work — e.g. a retrospective
// by-week run — with status, progress, and cost. Read-only; refreshable.
const STATUS_TONE: Record<string, string> = {
  completed: 'bg-emerald-100 text-emerald-800',
  running: 'bg-accent/15 text-accent',
  pending: 'bg-slate-100 text-slate-500',
  paused: 'bg-amber-100 text-amber-800',
  failed: 'bg-red-100 text-red-700',
}

// Friendly labels for the job-type strings the API emits (ADR-013/040).
const TYPE_LABEL: Record<string, string> = {
  summarization: 'Summary',
  scheduled: 'Scheduled summary',
  backfill: 'Retrospective backfill',
  sync: 'Message sync',
  wiki_synthesis: 'Wiki synthesis',
  regenerate: 'Regenerate',
}

export function Jobs() {
  const { client } = useAuth()
  const [jobs, setJobs] = useState<Job[] | null>(null)
  const [busy, setBusy] = useState(false)

  const load = useCallback(async () => {
    if (!client) return
    setBusy(true)
    try {
      setJobs(await client.listJobs())
    } catch {
      setJobs([])
    } finally {
      setBusy(false)
    }
  }, [client])

  useEffect(() => {
    void load()
  }, [load])

  return (
    <div className="mx-auto max-w-2xl space-y-4">
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <div className="flex items-start justify-between gap-3">
          <div>
            <h2 className="font-semibold text-slate-800">Jobs</h2>
            <p className="mt-1 text-sm text-slate-500">
              Background and long-running work — like a retrospective "summarize by week" — with
              its status, progress, and cost.
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

        <ul className="mt-4 divide-y divide-slate-100">
          {jobs?.length === 0 && (
            <li className="py-3 text-sm text-slate-400">
              No jobs yet — they appear here when you run something long (e.g. summarize by week).
            </li>
          )}
          {jobs?.map((j) => (
            <li key={j.id} className="flex items-center justify-between gap-3 py-3 text-sm">
              <div className="min-w-0">
                <span className="text-slate-800">{TYPE_LABEL[j.job_type] ?? j.job_type}</span>
                {j.progress_total > 0 && (
                  <span className="ml-2 text-xs text-slate-400">
                    {j.progress_current}/{j.progress_total}
                  </span>
                )}
                {j.cost_micros > 0 && (
                  <span className="ml-2 text-xs text-slate-400">
                    ${(j.cost_micros / 1_000_000).toFixed(4)}
                  </span>
                )}
                {j.failure_reason && (
                  <span className="ml-2 text-xs text-red-600">{j.failure_reason}</span>
                )}
                <div className="text-xs text-slate-400">
                  {new Date(j.updated_at * 1000).toLocaleString()}
                </div>
              </div>
              <span
                className={`shrink-0 rounded px-1.5 py-0.5 text-xs font-medium ${
                  STATUS_TONE[j.status] ?? 'bg-slate-100 text-slate-500'
                }`}
              >
                {j.status}
              </span>
            </li>
          ))}
        </ul>
      </div>
    </div>
  )
}
