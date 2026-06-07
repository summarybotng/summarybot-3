import { useState } from 'react'
import { useAuth } from '../auth'
import { ApiError } from '../api'
import type { LlmConfig } from '../types'

// Per-tenant LLM settings (ADR-125 Phase 2a). Tenant config is keyed by tenant,
// and editing requires ManageSettings on it — so this screen also lets you
// claim/provision a tenant (you become its Owner) when you aren't a member yet,
// which is the path to a working demo without a separate admin UI.
export function Settings() {
  const { client } = useAuth()
  const [tenant, setTenant] = useState('acme')
  const [loaded, setLoaded] = useState<LlmConfig | null>(null)
  const [needsClaim, setNeedsClaim] = useState(false)
  const [baseUrl, setBaseUrl] = useState('')
  const [model, setModel] = useState('')
  const [msg, setMsg] = useState<string | null>(null)

  async function load() {
    if (!client) return
    setMsg(null)
    setNeedsClaim(false)
    setLoaded(null)
    try {
      const cfg = await client.getLlmConfig(tenant.trim())
      setLoaded(cfg)
      setBaseUrl(cfg.base_url ?? '')
      setModel(cfg.model ?? '')
    } catch (e) {
      if (e instanceof ApiError && e.status === 403) {
        setNeedsClaim(true) // not a member → offer to claim/create it
      } else if (e instanceof ApiError && e.status === 404) {
        setMsg('No such tenant route — provision it below.')
        setNeedsClaim(true)
      } else {
        setMsg('Failed to load config.')
      }
    }
  }

  async function claim() {
    if (!client) return
    setMsg(null)
    try {
      await client.provisionTenant(tenant.trim())
      setMsg(`Tenant "${tenant.trim()}" created — you are its owner.`)
      await load()
    } catch (e) {
      setMsg(e instanceof ApiError && e.status === 409 ? 'Tenant exists and you are not a member.' : 'Provision failed.')
    }
  }

  async function save() {
    if (!client) return
    setMsg(null)
    try {
      const cfg = await client.setLlmConfig(tenant.trim(), {
        base_url: baseUrl.trim() || null,
        model: model.trim() || null,
      })
      setLoaded(cfg)
      setMsg('Saved. New summaries for this tenant will use it.')
    } catch (e) {
      setMsg(e instanceof ApiError && e.status === 400 ? 'base_url must be an http(s) URL.' : 'Save failed.')
    }
  }

  async function clear() {
    if (!client) return
    await client.clearLlmConfig(tenant.trim())
    setBaseUrl('')
    setModel('')
    setLoaded({ base_url: null, model: null })
    setMsg('Cleared — reverted to the platform default.')
  }

  return (
    <div className="mx-auto max-w-2xl space-y-6">
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <h2 className="font-semibold text-slate-800">LLM provider (per tenant)</h2>
        <p className="mt-1 text-sm text-slate-500">
          Point summarization at your own OpenAI-compatible endpoint (e.g. a self-hosted
          Ollama/LM Studio). Leave blank to use the platform default.
        </p>

        <div className="mt-4 flex gap-2">
          <input
            value={tenant}
            onChange={(e) => setTenant(e.target.value)}
            placeholder="tenant id"
            className="flex-1 rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
          />
          <button
            onClick={() => void load()}
            className="rounded-lg border border-slate-300 bg-white px-4 text-sm font-medium"
          >
            Load
          </button>
        </div>

        {needsClaim && (
          <button
            onClick={() => void claim()}
            className="mt-3 w-full rounded-lg border border-accent px-4 py-2 text-sm font-medium text-accent"
          >
            Claim / create tenant "{tenant.trim()}" (become owner)
          </button>
        )}

        {loaded && (
          <div className="mt-4 space-y-3">
            <div>
              <label className="text-sm font-medium text-slate-700">Base URL</label>
              <input
                value={baseUrl}
                onChange={(e) => setBaseUrl(e.target.value)}
                placeholder="http://mac-mini.local:11434/v1"
                className="mt-1 w-full rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
              />
            </div>
            <div>
              <label className="text-sm font-medium text-slate-700">Model</label>
              <input
                value={model}
                onChange={(e) => setModel(e.target.value)}
                placeholder="llama3.1"
                className="mt-1 w-full rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
              />
            </div>
            <div className="flex gap-2">
              <button
                onClick={() => void save()}
                className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-fg"
              >
                Save
              </button>
              <button
                onClick={() => void clear()}
                className="rounded-lg border border-slate-300 px-4 py-2 text-sm"
              >
                Clear
              </button>
            </div>
            <p className="text-xs text-slate-400">
              Keyless for now — bring-your-own-key for hosted providers is a follow-up (ADR-125 Phase 2b).
            </p>
          </div>
        )}

        {msg && <p className="mt-3 text-sm text-slate-600">{msg}</p>}
      </div>
    </div>
  )
}
