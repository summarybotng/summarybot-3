import { useEffect, useState } from 'react'
import { useAuth } from '../auth'
import { ApiError } from '../api'
import type { ConnectionStatus, SourceChannel, SourceSync, Summary } from '../types'

type Platform = 'discord' | 'slack'

interface Copy {
  title: string
  tokenHelp: React.ReactNode
  tokenPlaceholder: string
  needsScope: boolean
  scopePlaceholder: string
}

const COPY: Record<Platform, Copy> = {
  discord: {
    title: 'Discord',
    tokenHelp: (
      <>
        Create a bot in the Discord Developer Portal, invite it to your server with{' '}
        <em>Read Messages / Message History</em>, and paste its token.
      </>
    ),
    tokenPlaceholder: 'bot token',
    needsScope: true,
    scopePlaceholder: 'guild (server) id',
  },
  slack: {
    title: 'Slack',
    tokenHelp: (
      <>
        Create a Slack app, add the <em>channels:history</em> + <em>channels:read</em> bot scopes,
        install it, and paste the bot token (<code className="rounded bg-slate-100 px-1">xoxb-…</code>).
        Invite the bot to each channel you want summarized.
      </>
    ),
    tokenPlaceholder: 'xoxb-… bot token',
    needsScope: false,
    scopePlaceholder: '',
  },
}

// Live source ingestion (ADR-128). Generic over Discord/Slack: store a bot token
// (encrypted server-side), sync a source's recent messages into the store, then
// summarize each synced channel via the existing summarize-now path. Requires the
// server built with the platform's feature — otherwise `supported` is false and
// we show a hint.
export function Source({ platform }: { platform: Platform }) {
  const { client } = useAuth()
  const copy = COPY[platform]
  const [status, setStatus] = useState<ConnectionStatus | null>(null)
  const [token, setToken] = useState('')
  const [savingToken, setSavingToken] = useState(false)

  const [scopeId, setScopeId] = useState('')
  const [channels, setChannels] = useState('')
  const [directory, setDirectory] = useState<SourceChannel[] | null>(null)
  const [loadingDir, setLoadingDir] = useState(false)
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [lookbackDays, setLookbackDays] = useState(1)
  const [syncing, setSyncing] = useState(false)
  const [result, setResult] = useState<SourceSync | null>(null)
  const [err, setErr] = useState<string | null>(null)

  const [summarizing, setSummarizing] = useState<string | null>(null)
  const [summary, setSummary] = useState<Summary | null>(null)
  const [summaryNote, setSummaryNote] = useState<string | null>(null)

  // Reset transient state when switching platform tab.
  useEffect(() => {
    setStatus(null)
    setResult(null)
    setSummary(null)
    setSummaryNote(null)
    setErr(null)
    setDirectory(null)
    setSelected(new Set())
    if (!client) return
    client
      .connectionStatus(platform)
      .then(setStatus)
      .catch(() => setStatus({ token_set: false, supported: true }))
  }, [client, platform])

  async function saveToken(e: React.FormEvent) {
    e.preventDefault()
    if (!client || !token.trim()) return
    setSavingToken(true)
    setErr(null)
    try {
      setStatus(await client.setConnectionToken(platform, token.trim()))
      setToken('')
    } catch (e) {
      setErr(e instanceof ApiError ? `Could not save token (${e.status})` : 'Could not save token.')
    } finally {
      setSavingToken(false)
    }
  }

  async function clearToken() {
    if (!client) return
    await client.clearConnectionToken(platform).catch(() => {})
    setStatus((s) => (s ? { ...s, token_set: false } : s))
  }

  async function sync(e: React.FormEvent) {
    e.preventDefault()
    if (!client || (copy.needsScope && !scopeId.trim())) return
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
      setResult(
        await client.syncSource(platform, lookbackDays * 86400, scopeId.trim() || undefined, chans),
      )
    } catch (e) {
      setErr(
        e instanceof ApiError ? `Sync failed (${e.status}): ${e.message.slice(0, 200)}` : 'Sync failed.',
      )
    } finally {
      setSyncing(false)
    }
  }

  async function loadDirectory() {
    if (!client) return
    setLoadingDir(true)
    setErr(null)
    try {
      const dir = await client.sourceChannels(platform, scopeId.trim() || undefined)
      setDirectory(dir)
    } catch (e) {
      setDirectory(null)
      setErr(
        e instanceof ApiError
          ? `Couldn't load channels (${e.status}): ${e.message.slice(0, 200)}`
          : 'Could not load channels.',
      )
    } finally {
      setLoadingDir(false)
    }
  }

  // Toggle a channel in the selection and mirror the selection into the
  // comma-separated `channels` field the sync form already uses.
  function toggleChannel(id: string) {
    setSelected((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      setChannels([...next].join(','))
      return next
    })
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

  if (status && !status.supported) {
    return (
      <div className="mx-auto max-w-2xl">
        <div className="rounded-xl bg-white p-4 text-sm text-slate-500 shadow-sm ring-1 ring-slate-200">
          {copy.title} ingestion isn't enabled on this server. Rebuild the API with{' '}
          <code className="rounded bg-slate-100 px-1">--features {platform}</code> to fetch live{' '}
          {copy.title} messages.
        </div>
      </div>
    )
  }

  const tokenSet = status?.token_set ?? false
  const canSync = tokenSet && (!copy.needsScope || scopeId.trim().length > 0)

  return (
    <div className="mx-auto max-w-2xl space-y-4">
      {/* Bot token */}
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <h2 className="font-semibold text-slate-800">{copy.title} bot token</h2>
        <p className="mt-1 text-sm text-slate-500">
          {copy.tokenHelp} It's stored encrypted and never shown again.
        </p>
        {tokenSet && (
          <div className="mt-3 flex items-center gap-3 text-sm">
            <span className="rounded bg-green-100 px-2 py-0.5 text-green-700">token set</span>
            <button onClick={() => void clearToken()} className="text-slate-500 underline">
              clear
            </button>
          </div>
        )}
        <form onSubmit={saveToken} className="mt-3 flex gap-2">
          <input
            type="password"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            placeholder={tokenSet ? 'replace token…' : copy.tokenPlaceholder}
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
          Pull a {copy.title} source's recent messages into this workspace. Re-syncing is safe —
          already-stored messages are skipped.
        </p>
        <form onSubmit={sync} className="mt-4 space-y-3">
          {copy.needsScope && (
            <input
              value={scopeId}
              onChange={(e) => setScopeId(e.target.value)}
              placeholder={copy.scopePlaceholder}
              className="w-full rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
            />
          )}

          {/* Browse channels (WSP-006): list the server's channels grouped by
              category and pick them, instead of typing ids. */}
          <div className="rounded-lg border border-slate-200 bg-slate-50 p-3">
            <div className="flex items-center justify-between">
              <span className="text-sm font-medium text-slate-700">Browse channels</span>
              <button
                type="button"
                onClick={() => void loadDirectory()}
                disabled={loadingDir || !tokenSet || (copy.needsScope && !scopeId.trim())}
                className="rounded-lg border border-slate-300 px-3 py-1 text-xs disabled:opacity-50"
              >
                {loadingDir ? 'Loading…' : directory ? 'Refresh' : 'Load channels'}
              </button>
            </div>
            {!tokenSet && <p className="mt-1 text-xs text-slate-400">set a bot token first</p>}
            {copy.needsScope && tokenSet && !scopeId.trim() && (
              <p className="mt-1 text-xs text-slate-400">enter the {copy.scopePlaceholder} above</p>
            )}
            {directory && directory.length === 0 && (
              <p className="mt-2 text-xs text-slate-500">
                No channels found — check the bot is in the server and can read them.
              </p>
            )}
            {directory && directory.length > 0 && (
              <div className="mt-2 max-h-72 space-y-3 overflow-y-auto">
                {groupChannels(directory).map(([category, chans]) => (
                  <div key={category}>
                    <p className="text-xs font-semibold uppercase tracking-wide text-slate-400">
                      {category}
                    </p>
                    <div className="mt-1 grid grid-cols-1 gap-1 sm:grid-cols-2">
                      {chans.map((c) => (
                        <label key={c.id} className="flex items-center gap-2 text-sm text-slate-700">
                          <input
                            type="checkbox"
                            checked={selected.has(c.id)}
                            onChange={() => toggleChannel(c.id)}
                          />
                          <span className="truncate">#{c.name}</span>
                        </label>
                      ))}
                    </div>
                  </div>
                ))}
                <p className="text-xs text-slate-400">
                  {selected.size} selected · syncing with no selection fetches all channels.
                </p>
              </div>
            )}
          </div>

          <input
            value={channels}
            onChange={(e) => setChannels(e.target.value)}
            placeholder="or type channel ids, comma-separated (blank = all channels)"
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
            disabled={syncing || !canSync}
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

// Group a flat channel list by category (Discord), categories alphabetical with
// "Uncategorized" last, channels by name within each group.
function groupChannels(dir: SourceChannel[]): [string, SourceChannel[]][] {
  const groups = new Map<string, SourceChannel[]>()
  for (const c of dir) {
    const key = c.category ?? 'Uncategorized'
    const list = groups.get(key) ?? []
    list.push(c)
    groups.set(key, list)
  }
  return [...groups.entries()]
    .sort(([a], [b]) => {
      if (a === 'Uncategorized') return 1
      if (b === 'Uncategorized') return -1
      return a.localeCompare(b)
    })
    .map(([cat, chans]) => [cat, chans.sort((x, y) => x.name.localeCompare(y.name))])
}
