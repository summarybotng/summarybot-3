import { useEffect, useState } from 'react'
import { useAuth } from '../auth'
import { ApiError } from '../api'
import type { Budget, LlmConfig, MyTenant } from '../types'

const usd = (micros: number) => `$${(micros / 1_000_000).toFixed(2)}`

// Per-tenant LLM settings (ADR-125 Phase 2a). Tenant config is keyed by tenant,
// and editing requires ManageSettings on it — so this screen also lets you
// claim/provision a tenant (you become its Owner) when you aren't a member yet,
// which is the path to a working demo without a separate admin UI.
export function Settings() {
  const { client } = useAuth()
  const [tenant, setTenant] = useState('acme')
  const [myTenants, setMyTenants] = useState<MyTenant[]>([])
  const [loaded, setLoaded] = useState<LlmConfig | null>(null)
  const [needsClaim, setNeedsClaim] = useState(false)
  const [baseUrl, setBaseUrl] = useState('')
  const [model, setModel] = useState('')
  const [apiKey, setApiKey] = useState('')
  const [msg, setMsg] = useState<string | null>(null)
  const [budget, setBudget] = useState<Budget | null>(null)
  const [instructions, setInstructions] = useState('')
  const [instrMsg, setInstrMsg] = useState<string | null>(null)

  // Per-workspace summary instructions (SUM-007).
  useEffect(() => {
    if (!client) return
    client
      .getWorkspaceSettings()
      .then((s) => setInstructions(s.summary_instructions ?? ''))
      .catch(() => {})
  }, [client])

  // The tenants I belong to (TEN-001): discover them instead of typing an id.
  async function refreshMyTenants() {
    if (!client) return
    try {
      const mine = await client.listMyTenants()
      setMyTenants(mine)
      if (mine.length > 0) setTenant((cur) => (mine.some((t) => t.id === cur) ? cur : mine[0].id))
    } catch {
      /* best-effort; the typed input + claim still work */
    }
  }
  useEffect(() => {
    void refreshMyTenants()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client])

  async function saveInstructions() {
    if (!client) return
    setInstrMsg(null)
    try {
      const s = await client.setWorkspaceSettings(instructions.trim() || null)
      setInstructions(s.summary_instructions ?? '')
      setInstrMsg('Saved — new summaries for this workspace will use it.')
    } catch {
      setInstrMsg('Save failed.')
    }
  }
  const [limitUsd, setLimitUsd] = useState('')
  const [periodDays, setPeriodDays] = useState('30')

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
      // Budget is owner-only; load best-effort.
      try {
        const b = await client.getBudget(tenant.trim())
        setBudget(b)
        if (b.configured) {
          setLimitUsd((b.limit_micros / 1_000_000).toString())
          setPeriodDays(Math.round(b.period_secs / 86400).toString())
        }
      } catch {
        setBudget(null)
      }
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
      await refreshMyTenants()
      await load()
    } catch (e) {
      setMsg(e instanceof ApiError && e.status === 409 ? 'Tenant exists and you are not a member.' : 'Provision failed.')
    }
  }

  async function save() {
    if (!client) return
    setMsg(null)
    try {
      // Send api_key only when the admin typed one (omitting it keeps the
      // existing key; clearing is the dedicated button below).
      const body: { base_url: string | null; model: string | null; api_key?: string } = {
        base_url: baseUrl.trim() || null,
        model: model.trim() || null,
      }
      if (apiKey.trim()) body.api_key = apiKey.trim()
      const cfg = await client.setLlmConfig(tenant.trim(), body)
      setLoaded(cfg)
      setApiKey('')
      setMsg('Saved. New summaries for this tenant will use it.')
    } catch (e) {
      if (e instanceof ApiError && e.status === 400) {
        setMsg('Rejected: base_url must be an http(s) URL, or key encryption is not enabled on the server.')
      } else {
        setMsg('Save failed.')
      }
    }
  }

  async function removeKey() {
    if (!client) return
    const cfg = await client.setLlmConfig(tenant.trim(), { api_key: null })
    setLoaded(cfg)
    setApiKey('')
    setMsg('API key removed.')
  }

  async function clear() {
    if (!client) return
    await client.clearLlmConfig(tenant.trim())
    setBaseUrl('')
    setModel('')
    setApiKey('')
    setLoaded({ base_url: null, model: null, has_key: false })
    setMsg('Cleared — reverted to the platform default.')
  }

  async function saveBudget() {
    if (!client) return
    setMsg(null)
    const limit = Math.round(parseFloat(limitUsd || '0') * 1_000_000)
    const period = Math.round(parseFloat(periodDays || '0') * 86400)
    if (!(limit >= 0) || !(period >= 0)) {
      setMsg('Budget limit/period must be non-negative numbers.')
      return
    }
    const b = await client.setBudget(tenant.trim(), limit, period)
    setBudget(b)
    setMsg('Budget saved.')
  }

  async function removeBudget() {
    if (!client) return
    await client.clearBudget(tenant.trim())
    setBudget({
      configured: false,
      limit_micros: 0,
      period_secs: 0,
      spent_micros: 0,
      remaining_micros: 0,
      period_start: 0,
    })
    setMsg('Budget removed.')
  }

  return (
    <div className="mx-auto max-w-2xl space-y-6">
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <h2 className="font-semibold text-slate-800">Summary instructions (this workspace)</h2>
        <p className="mt-1 text-sm text-slate-500">
          Optional guidance appended to every summary prompt for this workspace — e.g. a
          perspective or focus. Leave blank for the default.
        </p>
        <textarea
          value={instructions}
          onChange={(e) => setInstructions(e.target.value)}
          rows={3}
          maxLength={2000}
          placeholder="e.g. Summarize from a product-management perspective; emphasize decisions, risks, and owners."
          className="mt-3 w-full rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
        />
        <div className="mt-2 flex items-center gap-3">
          <button
            onClick={() => void saveInstructions()}
            className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-fg"
          >
            Save instructions
          </button>
          {instrMsg && <span className="text-sm text-slate-600">{instrMsg}</span>}
        </div>
      </div>

      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <h2 className="font-semibold text-slate-800">LLM provider (per tenant)</h2>
        <p className="mt-1 text-sm text-slate-500">
          Point summarization at your own OpenAI-compatible endpoint (e.g. a self-hosted
          Ollama/LM Studio). Leave blank to use the platform default.
        </p>

        {/* Discover the tenants you belong to (TEN-001) — pick one instead of
            typing an id. The typed input below stays for joining/creating a new one. */}
        {myTenants.length > 0 && (
          <div className="mt-4">
            <p className="text-xs font-medium uppercase tracking-wide text-slate-400">Your tenants</p>
            <div className="mt-1 flex flex-wrap gap-1">
              {myTenants.map((t) => {
                const on = t.id === tenant.trim()
                return (
                  <button
                    key={t.id}
                    onClick={() => {
                      setTenant(t.id)
                      void load()
                    }}
                    className={`rounded-full px-2.5 py-1 text-xs ring-1 ${
                      on ? 'bg-accent text-accent-fg ring-accent' : 'bg-white text-slate-600 ring-slate-300'
                    }`}
                    title={`role: ${t.role}`}
                  >
                    {t.name} · {t.role}
                  </button>
                )
              })}
            </div>
          </div>
        )}

        <div className="mt-4 flex gap-2">
          <input
            value={tenant}
            onChange={(e) => setTenant(e.target.value)}
            placeholder={myTenants.length ? 'or type another tenant id to join/create' : 'tenant id'}
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
            <div>
              <label className="text-sm font-medium text-slate-700">
                API key{' '}
                <span className={loaded.has_key ? 'text-green-600' : 'text-slate-400'}>
                  ({loaded.has_key ? 'a key is set' : 'none'})
                </span>
              </label>
              <div className="mt-1 flex gap-2">
                <input
                  type="password"
                  value={apiKey}
                  onChange={(e) => setApiKey(e.target.value)}
                  placeholder={loaded.has_key ? 'leave blank to keep' : 'sk-… (for a hosted provider)'}
                  className="flex-1 rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
                />
                {loaded.has_key && (
                  <button
                    onClick={() => void removeKey()}
                    className="rounded-lg border border-slate-300 px-3 text-sm"
                  >
                    Remove
                  </button>
                )}
              </div>
              <p className="mt-1 text-xs text-slate-400">
                Stored encrypted (AES-256-GCM); never shown again. Needs key encryption enabled on
                the server.
              </p>
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
                Clear all
              </button>
            </div>
          </div>
        )}

        {msg && <p className="mt-3 text-sm text-slate-600">{msg}</p>}
      </div>

      {/* Budget (operator-lent key). Owner-only; shown when accessible. */}
      {budget && (
        <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
          <h2 className="font-semibold text-slate-800">Budget (operator-lent key)</h2>
          <p className="mt-1 text-sm text-slate-500">
            Cap this tenant's spend on the platform key. Summaries are refused once the limit is
            reached, until the window rolls.
          </p>

          {budget.configured && (
            <p className="mt-3 text-sm text-slate-600">
              Spent <span className="font-medium">{usd(budget.spent_micros)}</span> of{' '}
              <span className="font-medium">{usd(budget.limit_micros)}</span> ·{' '}
              <span className="text-green-600">{usd(budget.remaining_micros)} left</span>
            </p>
          )}

          <div className="mt-3 grid grid-cols-2 gap-2">
            <div>
              <label className="text-sm font-medium text-slate-700">Limit (USD)</label>
              <input
                value={limitUsd}
                onChange={(e) => setLimitUsd(e.target.value)}
                placeholder="10.00"
                className="mt-1 w-full rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
              />
            </div>
            <div>
              <label className="text-sm font-medium text-slate-700">Period (days)</label>
              <input
                value={periodDays}
                onChange={(e) => setPeriodDays(e.target.value)}
                placeholder="30"
                className="mt-1 w-full rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
              />
            </div>
          </div>
          <div className="mt-3 flex gap-2">
            <button
              onClick={() => void saveBudget()}
              className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-fg"
            >
              Save budget
            </button>
            {budget.configured && (
              <button
                onClick={() => void removeBudget()}
                className="rounded-lg border border-slate-300 px-4 py-2 text-sm"
              >
                Remove budget
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  )
}
