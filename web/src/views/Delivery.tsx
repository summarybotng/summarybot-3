import { useEffect, useState } from 'react'
import { useAuth } from '../auth'
import { ApiError } from '../api'
import type { Destination, Plugin } from '../types'

// Summary delivery destinations (DSH-010/011; ADR-126 plugin sinks). Beyond the
// always-on dashboard, a workspace can fan summaries out to sink plugins
// compiled into this build (webhook, Confluence, …). The form is rendered from
// each plugin's config schema; secret fields are stored encrypted and never
// returned — the list shows only a non-secret hint.
export function Delivery() {
  const { client } = useAuth()
  const [plugins, setPlugins] = useState<Plugin[]>([])
  const [dests, setDests] = useState<Destination[]>([])
  const [kind, setKind] = useState('')
  const [form, setForm] = useState<Record<string, string>>({})
  const [busy, setBusy] = useState(false)
  const [msg, setMsg] = useState<string | null>(null)
  const [testing, setTesting] = useState<string | null>(null)

  async function load() {
    if (!client) return
    try {
      const [pl, ds] = await Promise.all([client.listPlugins(), client.listDestinations()])
      setPlugins(pl)
      setDests(ds)
      if (pl.length && !kind) setKind(pl[0].id)
    } catch {
      setMsg('Failed to load delivery settings.')
    }
  }

  useEffect(() => {
    void load()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client])

  const selected = plugins.find((p) => p.id === kind)

  async function add(e: React.FormEvent) {
    e.preventDefault()
    if (!client || !selected) return
    setBusy(true)
    setMsg(null)
    try {
      await client.addDestination(kind, form)
      setForm({})
      setMsg(`${selected.display_name} destination added. New summaries will be delivered to it.`)
      await load()
    } catch (e) {
      setMsg(
        e instanceof ApiError && e.status === 400
          ? `Rejected: ${e.message.slice(0, 200)}`
          : 'Failed to add destination.',
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

  const requiredMissing = selected?.fields.some((f) => f.required && !form[f.name]?.trim()) ?? true

  return (
    <div className="mx-auto max-w-2xl space-y-6">
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <h2 className="font-semibold text-slate-800">Delivery destinations</h2>
        <p className="mt-1 text-sm text-slate-500">
          Every summary is always saved to the dashboard. Add a destination to also publish
          summaries elsewhere — you set only the target (channel / space / folder) here. Account
          credentials are configured once per tenant on the{' '}
          <span className="font-medium">Plugins</span> tab; a plugin a tenant has disabled is refused.
        </p>

        {plugins.length === 0 ? (
          <p className="mt-4 rounded-lg bg-slate-50 p-3 text-sm text-slate-500">
            No delivery plugins are enabled in this build. Rebuild the server with a plugin feature
            (e.g. <code className="rounded bg-slate-100 px-1">--features http-llm,confluence</code>).
          </p>
        ) : (
          <form onSubmit={add} className="mt-4 space-y-3">
            <div>
              <label className="text-sm font-medium text-slate-700">Type</label>
              <select
                value={kind}
                onChange={(e) => {
                  setKind(e.target.value)
                  setForm({})
                }}
                className="mt-1 w-full rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
              >
                {plugins.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.display_name}
                  </option>
                ))}
              </select>
            </div>

            {selected?.fields.map((f) => (
              <div key={f.name}>
                <label className="text-sm font-medium text-slate-700">
                  {f.label}
                  {f.required && <span className="text-red-500"> *</span>}
                </label>
                <input
                  type={f.secret ? 'password' : 'text'}
                  value={form[f.name] ?? ''}
                  onChange={(e) => setForm((s) => ({ ...s, [f.name]: e.target.value }))}
                  className="mt-1 w-full rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
                />
              </div>
            ))}

            <button
              type="submit"
              disabled={busy || requiredMissing}
              className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-fg disabled:opacity-50"
            >
              {busy ? 'Adding…' : `Add ${selected?.display_name ?? 'destination'}`}
            </button>
          </form>
        )}

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
                  {d.hint ?? '(config hidden)'}
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
