import { useCallback, useEffect, useState } from 'react'
import { useAuth } from '../auth'
import type { Feed } from '../types'

// Feeds (ADR-133 A4): RSS feeds of a workspace's summaries at a public,
// token-gated URL that RSS readers can subscribe to.

export function Feeds() {
  const { client } = useAuth()
  const [feeds, setFeeds] = useState<Feed[] | null>(null)
  const [title, setTitle] = useState('')
  const [channel, setChannel] = useState('')
  const [isPublic, setIsPublic] = useState(true)
  const [busy, setBusy] = useState(false)
  const [copied, setCopied] = useState<string | null>(null)

  const load = useCallback(async () => {
    if (!client) return
    try {
      setFeeds(await client.listFeeds())
    } catch {
      setFeeds([])
    }
  }, [client])

  useEffect(() => {
    void load()
  }, [load])

  async function create() {
    if (!client) return
    setBusy(true)
    try {
      await client.createFeed({
        title: title.trim() || null,
        channel_id: channel.trim() || null,
        is_public: isPublic,
      })
      setTitle('')
      setChannel('')
      await load()
    } finally {
      setBusy(false)
    }
  }

  async function remove(id: string) {
    if (!client) return
    await client.deleteFeed(id)
    await load()
  }

  function fullUrl(path: string): string {
    return `${window.location.origin}${path}`
  }

  return (
    <div className="mx-auto max-w-3xl space-y-4">
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <h2 className="font-semibold text-slate-800">Feeds</h2>
        <p className="mt-1 text-sm text-slate-500">
          RSS feeds of your summaries at a public, unguessable URL — subscribe in any reader.
        </p>
        <div className="mt-3 flex flex-wrap gap-2">
          <input
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            placeholder="Feed title (optional)"
            className="min-w-[12rem] flex-1 rounded-lg border border-slate-300 px-3 py-2 text-sm"
          />
          <input
            value={channel}
            onChange={(e) => setChannel(e.target.value)}
            placeholder="Channel id (optional — all if blank)"
            className="w-56 rounded-lg border border-slate-300 px-3 py-2 text-sm"
          />
          <label className="flex items-center gap-2 text-sm text-slate-600">
            <input type="checkbox" checked={isPublic} onChange={(e) => setIsPublic(e.target.checked)} />
            Public
          </label>
          <button
            onClick={create}
            disabled={busy}
            className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-fg disabled:opacity-50"
          >
            Create feed
          </button>
        </div>
      </div>

      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        {feeds?.length === 0 && <p className="py-3 text-sm text-slate-400">No feeds yet.</p>}
        <ul className="divide-y divide-slate-100">
          {feeds?.map((f) => (
            <li key={f.id} className="py-3 text-sm">
              <div className="flex items-center justify-between gap-3">
                <div className="min-w-0">
                  <span className="font-medium text-slate-800">{f.title || '(untitled feed)'}</span>
                  <span className="ml-2 rounded bg-slate-100 px-1.5 py-0.5 text-xs text-slate-500">
                    {f.feed_type.toUpperCase()}
                  </span>
                  <span
                    className={`ml-1 rounded px-1.5 py-0.5 text-xs ${
                      f.is_public ? 'bg-emerald-100 text-emerald-700' : 'bg-amber-100 text-amber-700'
                    }`}
                  >
                    {f.is_public ? 'Public' : 'Private'}
                  </span>
                  <span className="ml-2 text-xs text-slate-400">
                    {f.channel_id ? `#${f.channel_id}` : 'all channels'} · {f.access_count} accesses
                  </span>
                </div>
                <div className="flex shrink-0 gap-2">
                  <button
                    onClick={() => {
                      void navigator.clipboard?.writeText(fullUrl(f.url))
                      setCopied(f.id)
                      setTimeout(() => setCopied((c) => (c === f.id ? null : c)), 1500)
                    }}
                    className="rounded border border-slate-300 px-2 py-0.5 text-xs"
                  >
                    {copied === f.id ? 'Copied!' : 'Copy URL'}
                  </button>
                  <a
                    href={f.url}
                    target="_blank"
                    rel="noreferrer"
                    className="rounded border border-slate-300 px-2 py-0.5 text-xs"
                  >
                    Open
                  </a>
                  <button
                    onClick={() => void remove(f.id)}
                    className="rounded border border-red-200 px-2 py-0.5 text-xs text-red-600"
                  >
                    Delete
                  </button>
                </div>
              </div>
              <div className="mt-1 truncate font-mono text-xs text-slate-400">{fullUrl(f.url)}</div>
            </li>
          ))}
        </ul>
      </div>
    </div>
  )
}
