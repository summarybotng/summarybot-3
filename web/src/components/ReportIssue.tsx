import { useState } from 'react'
import { useAuth } from '../auth'

// Global "Report a problem" affordance (ADR-039): a button that opens a modal to
// submit a categorized problem report, auto-capturing the current page.

const CATEGORY_LABELS: Record<string, string> = {
  summary_quality: 'Summary quality',
  missing_summary: 'Missing summary',
  parameter_mismatch: 'Parameter mismatch',
  feed_issue: 'Feed issue',
  schedule_failure: 'Schedule failure',
  ui_bug: 'UI bug',
  performance: 'Performance',
  other: 'Other',
}
const CATEGORIES = Object.keys(CATEGORY_LABELS)

const SEVERITY_LABELS: Record<string, string> = {
  low: 'Low — minor / cosmetic',
  medium: 'Medium — noticeable',
  high: 'High — blocks a task',
  critical: 'Critical — broken / data loss',
}
const SEVERITIES = Object.keys(SEVERITY_LABELS)

export function ReportIssue({ currentTab }: { currentTab: string }) {
  const { client } = useAuth()
  const [open, setOpen] = useState(false)
  const [category, setCategory] = useState('other')
  const [severity, setSeverity] = useState('medium')
  const [description, setDescription] = useState('')
  const [busy, setBusy] = useState(false)
  const [done, setDone] = useState(false)

  async function submit() {
    if (!client || !description.trim()) return
    setBusy(true)
    try {
      await client.createIssue({
        category,
        severity,
        description,
        // Capture the real address bar (incl. hash route) and browser, like v2.
        page_url: typeof window !== 'undefined' ? window.location.href : `#${currentTab}`,
        browser: typeof navigator !== 'undefined' ? navigator.userAgent : undefined,
      })
      setDone(true)
      setDescription('')
      setTimeout(() => {
        setDone(false)
        setOpen(false)
      }, 1200)
    } finally {
      setBusy(false)
    }
  }

  return (
    <>
      <button
        onClick={() => setOpen(true)}
        className="border-t border-slate-100 px-4 py-2 text-left text-xs text-slate-500 hover:text-slate-800"
      >
        🐞 Report a problem
      </button>

      {open && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/30 p-4"
          onClick={() => setOpen(false)}
        >
          <div
            className="w-full max-w-md rounded-2xl bg-white p-5 shadow-lg"
            onClick={(e) => e.stopPropagation()}
          >
            <h3 className="font-semibold text-slate-800">Report a problem</h3>
            <p className="mt-1 text-xs text-slate-500">
              Tell us what went wrong — we capture the page you're on automatically.
            </p>
            <div className="mt-3 flex gap-2">
              <div className="flex-1">
                <label className="block text-sm font-medium text-slate-700">Category</label>
                <select
                  value={category}
                  onChange={(e) => setCategory(e.target.value)}
                  className="mt-1 w-full rounded-lg border border-slate-300 px-2 py-2 text-sm"
                >
                  {CATEGORIES.map((c) => (
                    <option key={c} value={c}>
                      {CATEGORY_LABELS[c]}
                    </option>
                  ))}
                </select>
              </div>
              <div className="flex-1">
                <label className="block text-sm font-medium text-slate-700">Severity</label>
                <select
                  value={severity}
                  onChange={(e) => setSeverity(e.target.value)}
                  className="mt-1 w-full rounded-lg border border-slate-300 px-2 py-2 text-sm"
                >
                  {SEVERITIES.map((s) => (
                    <option key={s} value={s}>
                      {SEVERITY_LABELS[s]}
                    </option>
                  ))}
                </select>
              </div>
            </div>
            <textarea
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              rows={4}
              placeholder="What happened? What did you expect?"
              className="mt-2 w-full resize-y rounded-lg border border-slate-300 p-2 text-sm outline-none focus:border-accent"
            />
            <div className="mt-3 flex justify-end gap-2">
              <button
                onClick={() => setOpen(false)}
                className="rounded-lg border border-slate-300 px-3 py-1.5 text-sm"
              >
                Cancel
              </button>
              <button
                onClick={submit}
                disabled={busy || !description.trim()}
                className="rounded-lg bg-accent px-4 py-1.5 text-sm font-medium text-accent-fg disabled:opacity-50"
              >
                {done ? 'Thanks! ✓' : busy ? 'Sending…' : 'Submit'}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  )
}
