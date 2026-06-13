import { useCallback, useEffect, useState } from 'react'
import { useAuth } from '../auth'
import { ApiError } from '../api'
import type { ChatCoverage, CoverageGap, Summary, WhatsappImport } from '../types'

// WhatsApp export ingestion (WHA-001). The browser sends the chosen file as the
// raw request body; the server unzips, parses, anonymizes, dedups and stores.
export function Whatsapp() {
  const { client } = useAuth()
  const [file, setFile] = useState<File | null>(null)
  const [chat, setChat] = useState('')
  // Default the timezone to the browser's; the date format is inferred from the
  // file (with this locale as a fallback for ambiguous dates), so no picker.
  const [tz, setTz] = useState(
    () => Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC',
  )
  const [busy, setBusy] = useState(false)
  const [result, setResult] = useState<WhatsappImport | null>(null)
  const [err, setErr] = useState<string | null>(null)
  const [summarizing, setSummarizing] = useState(false)
  const [summary, setSummary] = useState<Summary | null>(null)
  const [summaryNote, setSummaryNote] = useState<string | null>(null)
  const [chats, setChats] = useState<ChatCoverage[] | null>(null)

  const loadCoverage = useCallback(async () => {
    if (!client) return
    try {
      setChats(await client.whatsappChats())
    } catch {
      // A missing/empty workspace just yields no coverage; leave it null.
    }
  }, [client])

  // Load the coverage overview on mount (and whenever the workspace changes).
  useEffect(() => {
    void loadCoverage()
  }, [loadCoverage])

  async function submit(e: React.FormEvent) {
    e.preventDefault()
    if (!client || !file || !chat.trim()) return
    setBusy(true)
    setErr(null)
    setResult(null)
    setSummary(null)
    setSummaryNote(null)
    try {
      // Locale fallback for genuinely ambiguous dates (US → month/day).
      const locale = Intl.DateTimeFormat().resolvedOptions().locale
      const hint = /(^en-US$)|(-US$)/i.test(locale) ? 'mdy' : 'dmy'
      setResult(await client.importWhatsapp(chat.trim(), tz.trim() || 'UTC', hint, file))
      void loadCoverage()
    } catch (e) {
      setErr(
        e instanceof ApiError
          ? `Import failed (${e.status}): ${e.message.slice(0, 200)}`
          : 'Import failed.',
      )
    } finally {
      setBusy(false)
    }
  }

  // Summarize the most recent slice of the imported chat (bounded so the local
  // model's context isn't blown; full-history map-reduce is future work).
  async function summarizeNow() {
    if (!client || !result) return
    setSummarizing(true)
    setSummary(null)
    setSummaryNote(null)
    try {
      const now = Math.floor(Date.now() / 1000)
      // Window: the last ~3 days of the chat's activity, relative to now.
      const lookback = result.date_end ? now - result.date_end + 3 * 86400 : 31_536_000
      const r = await client.summarizeChannelNow(result.chat_id, Math.max(lookback, 86400))
      if (r.produced && r.summary) setSummary(r.summary)
      else setSummaryNote('No summary produced — no substantial messages in the recent window.')
    } catch {
      setSummaryNote('Summarize failed.')
    } finally {
      setSummarizing(false)
    }
  }

  return (
    <div className="mx-auto max-w-2xl space-y-6">
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <h2 className="font-semibold text-slate-800">Import a WhatsApp chat</h2>
        <p className="mt-1 text-sm text-slate-500">
          In WhatsApp: open the chat → ⋮ / contact name → <em>Export chat</em>. Choose{' '}
          <em>Without media</em> for the fastest import. Upload the resulting{' '}
          <code className="rounded bg-slate-100 px-1">.zip</code> (or the{' '}
          <code className="rounded bg-slate-100 px-1">_chat.txt</code>) here. Re-uploading is safe —
          duplicates are skipped.
        </p>

        <form onSubmit={submit} className="mt-4 space-y-3">
          <input
            type="file"
            accept=".zip,.txt"
            onChange={(e) => setFile(e.target.files?.[0] ?? null)}
            className="block w-full text-sm text-slate-600 file:mr-3 file:rounded-lg file:border-0 file:bg-accent file:px-4 file:py-2 file:text-sm file:font-medium file:text-accent-fg"
          />
          <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
            <input
              value={chat}
              onChange={(e) => setChat(e.target.value)}
              placeholder="channel id (e.g. family-group)"
              className="rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
            />
            <input
              value={tz}
              onChange={(e) => setTz(e.target.value)}
              title="Auto-detected from your browser; edit if the chat is from another timezone"
              placeholder="timezone"
              className="rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
            />
          </div>
          <p className="text-xs text-slate-400">
            Timezone is auto-detected from your browser. The date format is read from the file
            automatically.
          </p>
          <button
            type="submit"
            disabled={busy || !file || !chat.trim()}
            className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-fg disabled:opacity-50"
          >
            {busy ? 'Importing…' : 'Import'}
          </button>
        </form>

        {err && <p className="mt-3 text-sm text-red-600">{err}</p>}
        {result && (
          <div className="mt-4 rounded-lg bg-accent/10 p-3 text-sm text-slate-700">
            Imported <span className="font-medium">{result.stored}</span> messages into{' '}
            <span className="font-medium">#{result.chat_id}</span>
            {result.duplicates > 0 && <> ({result.duplicates} duplicates skipped)</>} ·{' '}
            {result.new_participants} participants · {result.format} export.
            <div className="mt-2">
              <button
                onClick={() => void summarizeNow()}
                disabled={summarizing}
                className="rounded-lg bg-accent px-4 py-1.5 text-sm font-medium text-accent-fg disabled:opacity-50"
              >
                {summarizing ? 'Summarizing…' : 'Summarize now'}
              </button>
              <span className="ml-2 text-xs text-slate-500">most recent activity</span>
            </div>
          </div>
        )}

        {summaryNote && <p className="mt-3 text-sm text-slate-600">{summaryNote}</p>}
        {summary && (
          <div className="mt-4 rounded-lg border border-slate-200 p-3">
            <p className="text-sm text-slate-800">{summary.text}</p>
            {summary.key_points.length > 0 && (
              <ul className="mt-2 list-disc pl-5 text-sm text-slate-600">
                {summary.key_points.map((k, i) => (
                  <li key={i}>{k}</li>
                ))}
              </ul>
            )}
            <p className="mt-2 text-xs text-slate-400">
              {summary.model}
              {summary.degraded && ' · degraded'} · saved to Summaries
            </p>
          </div>
        )}
      </div>

      <CoverageOverview chats={chats} />
    </div>
  )
}

// --- Coverage timeline (WHA-016/017; ADR-121) ---

const DAY = 86_400

function fmtDate(secs: number): string {
  return new Date(secs * 1000).toISOString().slice(0, 10)
}

function fmtDays(secs: number): string {
  const d = Math.round(secs / DAY)
  if (d >= 365) return `${(d / 365).toFixed(1)} yr`
  return `${d} day${d === 1 ? '' : 's'}`
}

const GAP_LABEL: Record<CoverageGap['kind'], string> = {
  before_join: 'Before our earliest export',
  between_imports: 'Between imports',
  after_last: 'Since the last export',
}

// The copy-ready instruction we want members to act on (WHA-019).
function askText(chat: string, g: CoverageGap): string {
  const range = `${fmtDate(g.start)} → ${fmtDate(g.end)}`
  if (g.kind === 'before_join') {
    return `We're missing #${chat} history before ${fmtDate(g.end)}. If you joined earlier, please export the chat from the beginning: in WhatsApp open #${chat} → ⋮ / contact name → Export chat → Without media, then upload the .zip here.`
  }
  if (g.kind === 'after_last') {
    return `We're missing #${chat} messages since ${fmtDate(g.start)}. Please re-export the most recent history: in WhatsApp open #${chat} → ⋮ / contact name → Export chat → Without media, then upload the .zip here.`
  }
  return `We're missing #${chat} messages for ${range}. If you have that period, please export the chat (WhatsApp → #${chat} → ⋮ → Export chat → Without media) and upload the .zip here.`
}

function CoverageOverview({ chats }: { chats: ChatCoverage[] | null }) {
  if (!chats) return null
  if (chats.length === 0) {
    return (
      <div className="rounded-xl bg-white p-4 text-sm text-slate-500 shadow-sm ring-1 ring-slate-200">
        No chats imported yet. Coverage and history gaps will appear here once you import a chat.
      </div>
    )
  }
  return (
    <div className="space-y-4">
      <h2 className="font-semibold text-slate-800">Coverage &amp; history gaps</h2>
      {chats.map((c) => (
        <ChatCoverageCard key={c.chat_id} chat={c} />
      ))}
    </div>
  )
}

function ChatCoverageCard({ chat }: { chat: ChatCoverage }) {
  const { coverage: cov } = chat
  const now = Math.floor(Date.now() / 1000)
  // Timeline domain: from the earliest known instant (a before_join gap can start
  // before the first covered message) to now.
  const starts = [cov.earliest, ...cov.gaps.map((g) => g.start)].filter(
    (v): v is number => v != null,
  )
  const domainStart = starts.length ? Math.min(...starts) : now - DAY
  const domainEnd = Math.max(now, cov.latest ?? now)
  const span = Math.max(domainEnd - domainStart, 1)
  const pct = (v: number) => `${(Math.max(0, Math.min(v, span)) / span) * 100}%`
  const coveragePct = Math.round((cov.covered_secs / span) * 100)
  const fillable = cov.gaps.filter((g) => g.can_fill)

  return (
    <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
      <div className="flex items-baseline justify-between">
        <h3 className="font-medium text-slate-800">#{chat.chat_id}</h3>
        <span className="text-xs text-slate-500">
          {chat.import_count} import{chat.import_count === 1 ? '' : 's'} ·{' '}
          {chat.message_count.toLocaleString()} messages
        </span>
      </div>

      {/* Covered (green) timeline with red gap overlays. */}
      <div className="relative mt-3 h-4 w-full overflow-hidden rounded bg-emerald-400/70">
        {cov.gaps.map((g, i) => (
          <div
            key={i}
            title={`${GAP_LABEL[g.kind]}: ${fmtDate(g.start)} → ${fmtDate(g.end)}`}
            className={`absolute top-0 h-full ${g.can_fill ? 'bg-amber-400' : 'bg-slate-300'}`}
            style={{ left: pct(g.start - domainStart), width: pct(g.end - g.start) }}
          />
        ))}
      </div>
      <div className="mt-1 flex justify-between text-xs text-slate-400">
        <span>{cov.earliest ? fmtDate(domainStart) : '—'}</span>
        <span>
          ~{coveragePct}% covered ({fmtDays(cov.covered_secs)})
        </span>
        <span>now</span>
      </div>

      {fillable.length > 0 ? (
        <div className="mt-3 space-y-2">
          <p className="text-xs font-medium text-slate-600">
            {fillable.length} fillable gap{fillable.length === 1 ? '' : 's'} — ask members to export:
          </p>
          {fillable.map((g, i) => (
            <GapAsk key={i} chat={chat.chat_id} gap={g} />
          ))}
        </div>
      ) : (
        <p className="mt-3 text-xs text-emerald-700">No fillable gaps — this chat is fully covered.</p>
      )}
    </div>
  )
}

function GapAsk({ chat, gap }: { chat: string; gap: CoverageGap }) {
  const [copied, setCopied] = useState(false)
  const text = askText(chat, gap)
  async function copy() {
    try {
      await navigator.clipboard.writeText(text)
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    } catch {
      /* clipboard may be unavailable; the text is shown regardless */
    }
  }
  return (
    <div className="rounded-lg border border-slate-200 bg-slate-50 p-2">
      <div className="flex items-center justify-between gap-2">
        <span className="text-xs font-medium text-slate-700">
          {GAP_LABEL[gap.kind]} · {fmtDate(gap.start)} → {fmtDate(gap.end)} ({fmtDays(gap.end - gap.start)})
        </span>
        <button
          onClick={() => void copy()}
          className="shrink-0 rounded bg-accent px-2 py-1 text-xs font-medium text-accent-fg"
        >
          {copied ? 'Copied!' : 'Copy ask'}
        </button>
      </div>
      <p className="mt-1 text-xs text-slate-500">{text}</p>
    </div>
  )
}
