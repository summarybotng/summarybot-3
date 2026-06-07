import { useState } from 'react'
import { useAuth } from '../auth'
import { ApiError } from '../api'
import type { WhatsappImport } from '../types'

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

  async function submit(e: React.FormEvent) {
    e.preventDefault()
    if (!client || !file || !chat.trim()) return
    setBusy(true)
    setErr(null)
    setResult(null)
    try {
      // Locale fallback for genuinely ambiguous dates (US → month/day).
      const locale = Intl.DateTimeFormat().resolvedOptions().locale
      const hint = /(^en-US$)|(-US$)/i.test(locale) ? 'mdy' : 'dmy'
      setResult(await client.importWhatsapp(chat.trim(), tz.trim() || 'UTC', hint, file))
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
            <div className="mt-1 text-xs text-slate-500">
              Now summarize it: create a schedule scoped to <code>#{result.chat_id}</code>, or
              trigger one with “Run now”.
            </div>
          </div>
        )}
      </div>
    </div>
  )
}
