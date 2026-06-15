import { useCallback, useEffect, useMemo, useState } from 'react'
import { useAuth } from '../auth'
import type { Job } from '../types'

// Jobs view (ADR-013/040; enriched per ADR-133 §B): long-running / background
// work with status buckets, type/status filters, and a per-job details panel
// (scope, schedule, covered window, produced summaries, timing). Read-only.

const STATUS_TONE: Record<string, string> = {
  completed: 'bg-emerald-100 text-emerald-800',
  running: 'bg-accent/15 text-accent',
  pending: 'bg-slate-100 text-slate-500',
  paused: 'bg-amber-100 text-amber-800',
  failed: 'bg-red-100 text-red-700',
}

const TYPE_LABEL: Record<string, string> = {
  summarization: 'Summary',
  scheduled: 'Scheduled summary',
  backfill: 'Retrospective backfill',
  sync: 'Message sync',
  wiki_synthesis: 'Wiki synthesis',
  regenerate: 'Regenerate',
}

const BUCKETS = ['running', 'pending', 'completed', 'failed', 'paused'] as const

function when(secs: number): string {
  return new Date(secs * 1000).toLocaleString()
}

function duration(j: Job): string | null {
  if (j.started_at == null || j.completed_at == null) return null
  const s = Math.max(0, j.completed_at - j.started_at)
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`
}

export function Jobs() {
  const { client } = useAuth()
  const [jobs, setJobs] = useState<Job[] | null>(null)
  const [busy, setBusy] = useState(false)
  const [typeFilter, setTypeFilter] = useState('all')
  const [statusFilter, setStatusFilter] = useState('all')
  const [open, setOpen] = useState<string | null>(null)

  const load = useCallback(async () => {
    if (!client) return
    setBusy(true)
    try {
      setJobs(await client.listJobs(200))
    } catch {
      setJobs([])
    } finally {
      setBusy(false)
    }
  }, [client])

  useEffect(() => {
    void load()
  }, [load])

  const counts = useMemo(() => {
    const c: Record<string, number> = {}
    for (const j of jobs ?? []) c[j.status] = (c[j.status] ?? 0) + 1
    return c
  }, [jobs])

  const types = useMemo(() => {
    const set = new Set((jobs ?? []).map((j) => j.job_type))
    return Array.from(set).sort()
  }, [jobs])

  const shown = (jobs ?? []).filter(
    (j) =>
      (typeFilter === 'all' || j.job_type === typeFilter) &&
      (statusFilter === 'all' || j.status === statusFilter),
  )

  return (
    <div className="mx-auto max-w-3xl space-y-4">
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <div className="flex items-start justify-between gap-3">
          <div>
            <h2 className="font-semibold text-slate-800">Jobs</h2>
            <p className="mt-1 text-sm text-slate-500">
              Background and long-running work — summaries, scheduled runs, syncs, backfills — with
              status, progress, cost, and what each one produced.
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

        {/* Status buckets */}
        <div className="mt-4 grid grid-cols-5 gap-2">
          {BUCKETS.map((s) => (
            <button
              key={s}
              onClick={() => setStatusFilter(statusFilter === s ? 'all' : s)}
              className={`rounded-lg px-2 py-2 text-center ring-1 ${
                statusFilter === s ? 'ring-accent' : 'ring-slate-200'
              }`}
            >
              <div className="text-lg font-semibold text-slate-800">{counts[s] ?? 0}</div>
              <div className="text-xs capitalize text-slate-400">{s}</div>
            </button>
          ))}
        </div>

        {/* Filters */}
        <div className="mt-3 flex flex-wrap items-center gap-2 text-sm">
          <select
            value={typeFilter}
            onChange={(e) => setTypeFilter(e.target.value)}
            className="rounded-lg border border-slate-300 px-2 py-1"
          >
            <option value="all">All types</option>
            {types.map((t) => (
              <option key={t} value={t}>
                {TYPE_LABEL[t] ?? t}
              </option>
            ))}
          </select>
          <select
            value={statusFilter}
            onChange={(e) => setStatusFilter(e.target.value)}
            className="rounded-lg border border-slate-300 px-2 py-1"
          >
            <option value="all">All statuses</option>
            {BUCKETS.map((s) => (
              <option key={s} value={s} className="capitalize">
                {s}
              </option>
            ))}
          </select>
          {(typeFilter !== 'all' || statusFilter !== 'all') && (
            <button
              onClick={() => {
                setTypeFilter('all')
                setStatusFilter('all')
              }}
              className="text-xs text-accent"
            >
              Clear filters
            </button>
          )}
          <span className="ml-auto text-xs text-slate-400">{shown.length} shown</span>
        </div>
      </div>

      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <ul className="divide-y divide-slate-100">
          {shown.length === 0 && (
            <li className="py-3 text-sm text-slate-400">
              No jobs{jobs?.length ? ' match the filters' : ' yet'}.
            </li>
          )}
          {shown.map((j) => (
            <li key={j.id} className="py-3 text-sm">
              <div className="flex items-center justify-between gap-3">
                <button onClick={() => setOpen(open === j.id ? null : j.id)} className="min-w-0 text-left">
                  <span className="font-medium text-slate-800">
                    {TYPE_LABEL[j.job_type] ?? j.job_type}
                  </span>
                  {j.scope && <span className="ml-2 text-xs text-slate-400">{j.scope}</span>}
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
                  <div className="text-xs text-slate-400">{when(j.created_at)}</div>
                </button>
                <span
                  className={`shrink-0 rounded px-1.5 py-0.5 text-xs font-medium ${
                    STATUS_TONE[j.status] ?? 'bg-slate-100 text-slate-600'
                  }`}
                >
                  {j.status}
                </span>
              </div>

              {open === j.id && (
                <dl className="mt-2 grid grid-cols-[8rem_1fr] gap-x-3 gap-y-1 text-xs text-slate-500">
                  {j.schedule_name && (
                    <>
                      <dt className="text-slate-400">Schedule</dt>
                      <dd className="font-mono">{j.schedule_name}</dd>
                    </>
                  )}
                  {j.creation_source && (
                    <>
                      <dt className="text-slate-400">Triggered by</dt>
                      <dd>{j.creation_source}</dd>
                    </>
                  )}
                  {j.date_end > j.date_start && (
                    <>
                      <dt className="text-slate-400">Covered</dt>
                      <dd>
                        {when(j.date_start)} — {when(j.date_end)}
                      </dd>
                    </>
                  )}
                  {duration(j) && (
                    <>
                      <dt className="text-slate-400">Duration</dt>
                      <dd>{duration(j)}</dd>
                    </>
                  )}
                  {j.failure_reason && (
                    <>
                      <dt className="text-slate-400">Failure</dt>
                      <dd className="text-red-600">{j.failure_reason}</dd>
                    </>
                  )}
                  {j.pause_reason && (
                    <>
                      <dt className="text-slate-400">Paused</dt>
                      <dd>{j.pause_reason}</dd>
                    </>
                  )}
                  {j.summary_ids.length > 0 && (
                    <>
                      <dt className="text-slate-400">Produced</dt>
                      <dd>
                        {j.summary_ids.length} summar{j.summary_ids.length === 1 ? 'y' : 'ies'}
                        <span className="ml-1 break-all font-mono text-slate-400">
                          ({j.summary_ids.join(', ')})
                        </span>
                      </dd>
                    </>
                  )}
                  <dt className="text-slate-400">ID</dt>
                  <dd className="break-all font-mono">{j.id}</dd>
                </dl>
              )}
            </li>
          ))}
        </ul>
      </div>
    </div>
  )
}
