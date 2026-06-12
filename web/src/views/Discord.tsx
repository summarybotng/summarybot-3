import { useEffect, useState } from 'react'
import { useAuth } from '../auth'
import { ApiError } from '../api'
import type { DiscordSync, Summary } from '../types'

// Discord live ingestion (ADR-128). Store a bot token (encrypted server-side),
// then sync a guild's recent messages into the store; from there the existing
// "summarize now" path produces a summary. Requires the server built with
// `--features discord` — otherwise the endpoints 404 and we show a hint.
export function Discord() {
  const { client } = useAuth()
  const [available, setAvailable] = useState<boolean | null>(null)
  const [tokenSet, setTokenSet] = useState(false)
  const [token, setToken] = useState('')
  const [savingToken, setSavingToken] = useState(false)

  const [guildId, setGuildId] = useState('')
  const [channels, setChannels] = useState('')
  const [lookbackDays, setLookbackDays] = useState(1)
  const [syncing, setSyncing] = useState(false)
  const [result, setResult] = useState<DiscordSync | null>(null)
  const [err, setErr] = useState<string | null>(null)

  const [summarizing, setSummarizing] = useState<string | null>(null)
  const [summary, setSummary] = useState<Summary | null>(null)
  const [summaryNote, setSummaryNote] = useState<string | null>(null)

  useEffect(() => {
    if (!client) return
    client
      .discordStatus()
      .then((s) => {
        setAvailable(true)
        setTokenSet(s.token_set)
      })
      .catch((e) => setAvailable(e instanceof ApiError && e.status === 404 ? false : true))
  }, [client])

  async function saveToken(e: React.FormEvent) {
    e.preventDefault()
    if (!client || !token.trim()) return
    setSavingToken(true)
    setErr(null)
    try {
      const s = await client.setDiscordToken(token.trim())
      setTokenSet(s.token_set)
      setToken('')
    } catch (e) {
      setErr(e instanceof ApiError ? `Could not save token (${e.status})` : 'Could not save token.')
    } finally {
      setSavingToken(false)
    }
  }

  async function clearToken() {
    if (!client) return
    await client.clearDiscordToken().catch(() => {})
    setTokenSet(false)
  }

  async function sync(e: React.FormEvent) {
    e.preventDefault()
    if (!client || !guildId.trim()) return
    setSyncing(true)
    setErr(null)
    setResult(null)
    setSummary(null)
    setSummaryNote(null)
    try {
      const chans = channels
        .split(',')
        .map((c) => c.trim())
        .filter(Boolean)
      setResult(await client.syncDiscord(guildId.trim(), lookbackDays * 86400, chans))
    } catch (e) {
      setErr(
        e instanceof ApiError ? `Sync failed (${e.status}): ${e.message.slice(0, 200)}` : 'Sync failed.',
      )
    } finally {
      setSyncing(false)
    }
  }

  async function summarizeChannel(channel: string) {
    if (!client) return
    setSummarizing(channel)
    setSummary(null)
    setSummaryNote(null)
    try {
      const r = await client.summarizeChannelNow(channel, lookbackDays * 86400)
      if (r.produced && r.summary) setSummary(r.summary)
      else setSummaryNote(`No summary for #${channel} — no substantial messages in the window.`)
    } catch {
      setSummaryNote('Summarize failed.')
    } finally {
      setSummarizing(null)
    }
  }

  if (available === false) {
    return (
      <div className="mx-auto max-w-2xl">
        <div className="rounded-xl bg-white p-4 text-sm text-slate-500 shadow-sm ring-1 ring-slate-200">
          Discord ingestion isn't enabled on this server. Rebuild the API with{' '}
          <code className="rounded bg-slate-100 px-1">--features discord</code> to fetch live Discord
          messages.
        </div>
      </div>
    )
  }

  return (
    <div className="mx-auto max-w-2xl space-y-4">
      {/* Bot token */}
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <h2 className="font-semibold text-slate-800">Discord bot token</h2>
        <p className="mt-1 text-sm text-slate-500">
          Create a bot in the Discord Developer Portal, invite it to your server with{' '}
          <em>Read Messages / Message History</em>, and paste its token. It's stored encrypted and
          never shown again.
        </p>
        {tokenSet ? (
          <div className="mt-3 flex items-center gap-3 text-sm">
            <span className="rounded bg-green-100 px-2 py-0.5 text-green-700">token set</span>
            <button onClick={() => void clearToken()} className="text-slate-500 underline">
              clear
            </button>
          </div>
        ) : null}
        <form onSubmit={saveToken} className="mt-3 flex gap-2">
          <input
            type="password"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            placeholder={tokenSet ? 'replace token…' : 'bot token'}
            className="flex-1 rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
          />
          <button
            type="submit"
            disabled={savingToken || !token.trim()}
            className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-fg disabled:opacity-50"
          >
            {savingToken ? 'Saving…' : 'Save'}
          </button>
        </form>
      </div>

      {/* Sync */}
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <h2 className="font-semibold text-slate-800">Sync messages</h2>
        <p className="mt-1 text-sm text-slate-500">
          Pull a server's recent messages into this workspace. Re-syncing is safe — already-stored
          messages are skipped.
        </p>
        <form onSubmit={sync} className="mt-4 space-y-3">
          <input
            value={guildId}
            onChange={(e) => setGuildId(e.target.value)}
            placeholder="guild (server) id"
            className="w-full rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
          />
          <input
            value={channels}
            onChange={(e) => setChannels(e.target.value)}
            placeholder="channel ids, comma-separated (blank = all text channels)"
            className="w-full rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
          />
          <label className="flex items-center gap-2 text-sm text-slate-600">
            Look back
            <input
              type="number"
              min={1}
              value={lookbackDays}
              onChange={(e) => setLookbackDays(Math.max(1, Number(e.target.value) || 1))}
              className="w-20 rounded-lg border border-slate-300 px-2 py-1 text-sm outline-none focus:border-accent"
            />
            day(s)
          </label>
          <button
            type="submit"
            disabled={syncing || !guildId.trim() || !tokenSet}
            className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-fg disabled:opacity-50"
          >
            {syncing ? 'Syncing…' : 'Sync now'}
          </button>
          {!tokenSet && <span className="ml-2 text-xs text-slate-400">set a bot token first</span>}
        </form>

        {err && <p className="mt-3 text-sm text-red-600">{err}</p>}
        {result && (
          <div className="mt-4 rounded-lg bg-accent/10 p-3 text-sm text-slate-700">
            Fetched <span className="font-medium">{result.fetched}</span> messages from{' '}
            <span className="font-medium">{result.channel_ids.length}</span> channel(s),{' '}
            <span className="font-medium">{result.stored}</span> newly stored.
            {result.errors.length > 0 && (
              <ul className="mt-2 list-disc pl-5 text-xs text-red-600">
                {result.errors.map((e, i) => (
                  <li key={i}>
                    #{e.channel}: {e.message}
                  </li>
                ))}
              </ul>
            )}
            {result.channel_ids.length > 0 && (
              <div className="mt-3 flex flex-wrap gap-2">
                {result.channel_ids.map((c) => (
                  <button
                    key={c}
                    onClick={() => void summarizeChannel(c)}
                    disabled={summarizing !== null}
                    className="rounded-lg bg-accent px-3 py-1.5 text-xs font-medium text-accent-fg disabled:opacity-50"
                  >
                    {summarizing === c ? 'Summarizing…' : `Summarize #${c}`}
                  </button>
                ))}
              </div>
            )}
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
    </div>
  )
}
