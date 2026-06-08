import { useEffect, useState } from 'react'
import { useAuth } from '../auth'
import { ApiError } from '../api'
import type { Destination } from '../types'

// Summary delivery destinations (DSH-010/011). Beyond the always-on dashboard, a
// workspace can fan summaries out to webhooks — generic, or Slack/Discord
// incoming-webhook URLs. The URL is a secret: it's stored encrypted and only a
// scheme+host hint is ever shown back.
export function Delivery() {
  const { client } = useAuth()
  const [dests, setDests] = useState<Destination[]>([])
  const [url, setUrl] = useState('')
  const [busy, setBusy] = useState(false)
  const [msg, setMsg] = useState<string | null>(null)
  const [testing, setTesting] = useState<string | null>(null)

  async function load() {
    if (!client) return
    try {
      setDests(await client.listDestinations())
    } catch {
      setMsg('Failed to load destinations.')
    }
  }

  useEffect(() => {
    void load()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client])

  async function add(e: React.FormEvent) {
    e.preventDefault()
    if (!client || !url.trim()) return
    setBusy(true)
    setMsg(null)
    try {
      await client.addWebhook(url.trim())
      setUrl('')
      setMsg('Webhook added. New summaries for this workspace will be delivered to it.')
      await load()
    } catch (e) {
      setMsg(
        e instanceof ApiError && e.status === 400
          ? `Rejected: ${e.message.slice(0, 160)}`
          : 'Failed to add webhook.',
      )
    } finally {
      setBusy(false)
    }
  }

  async function remove(id: string) {
    if (!client) return
    await client.deleteDestination(id).catch(() => {})
    await load()
  }

  async function test(id: string) {
    if (!client) return
    setTesting(id)
    setMsg(null)
    try {
      const r = await client.testDestination(id)
      setMsg(r.ok ? 'Test delivered successfully.' : `Test failed: ${r.detail ?? 'unknown error'}`)
    } catch {
      setMsg('Test request failed.')
    } finally {
      setTesting(null)
    }
  }

  return (
    <div className="mx-auto max-w-2xl space-y-6">
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <h2 className="font-semibold text-slate-800">Delivery destinations</h2>
        <p className="mt-1 text-sm text-slate-500">
          Every summary is always saved to the dashboard. Add a webhook to also push summaries to
          Slack or Discord (paste an <em>incoming webhook</em> URL) or any HTTP endpoint. The URL is
          stored encrypted and never shown again.
        </p>

        <form onSubmit={add} className="mt-4 flex gap-2">
          <input
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            placeholder="https://hooks.slack.com/services/…"
            className="flex-1 rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
          />
          <button
            type="submit"
            disabled={busy || !url.trim()}
            className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-fg disabled:opacity-50"
          >
            {busy ? 'Adding…' : 'Add webhook'}
          </button>
        </form>

        {msg && <p className="mt-3 text-sm text-slate-600">{msg}</p>}

        <ul className="mt-4 divide-y divide-slate-100">
          {dests.length === 0 && (
            <li className="py-3 text-sm text-slate-400">
              No external destinations yet — summaries go to the dashboard only.
            </li>
          )}
          {dests.map((d) => (
            <li key={d.id} className="flex items-center justify-between py-3">
              <div className="min-w-0">
                <span className="rounded bg-slate-100 px-1.5 py-0.5 text-xs font-medium text-slate-600">
                  {d.kind}
                </span>
                <span className="ml-2 truncate text-sm text-slate-700">
                  {d.hint ?? '(address unavailable)'}
                </span>
                {!d.enabled && <span className="ml-2 text-xs text-slate-400">disabled</span>}
              </div>
              <div className="flex shrink-0 gap-2">
                <button
                  onClick={() => void test(d.id)}
                  disabled={testing === d.id}
                  className="rounded-lg border border-slate-300 px-3 py-1 text-sm disabled:opacity-50"
                >
                  {testing === d.id ? 'Testing…' : 'Test'}
                </button>
                <button
                  onClick={() => void remove(d.id)}
                  className="rounded-lg border border-slate-300 px-3 py-1 text-sm text-red-600"
                >
                  Remove
                </button>
              </div>
            </li>
          ))}
        </ul>
      </div>
    </div>
  )
}
