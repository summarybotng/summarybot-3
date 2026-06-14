import { useCallback, useEffect, useState } from 'react'
import { useAuth } from '../auth'
import { ApiError } from '../api'
import type { TenantPlugin } from '../types'

// Per-tenant delivery plugin admin (ADR-126, two-layer model). A tenant admin
// enables a plugin and configures/connects its ACCOUNT credentials once here;
// workspaces then add destinations that pick only the non-secret target on the
// Delivery tab. Tenant is chosen by id (like Members); all actions require
// Admin/Owner — a 403 means insufficient role. Secrets are never shown back.
export function Plugins() {
  const { client } = useAuth()
  const [tenant, setTenant] = useState('acme')
  const [plugins, setPlugins] = useState<TenantPlugin[] | null>(null)
  const [err, setErr] = useState<string | null>(null)
  const [note, setNote] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const [isOperator, setIsOperator] = useState(false)
  const [connectInfo, setConnectInfo] = useState<{ uri: string; kinds: string[] } | null>(null)
  // Per-plugin draft of the tenant-scoped field values (secrets blank = keep).
  const [drafts, setDrafts] = useState<Record<string, Record<string, string>>>({})

  // OAuth setup guidance (the exact redirect URI to register + which providers
  // the server has client ids for).
  useEffect(() => {
    if (!client) return
    client
      .connectInfo()
      .then((i) => setConnectInfo({ uri: i.connect_redirect_uri, kinds: i.configured_kinds }))
      .catch(() => setConnectInfo(null))
  }, [client])

  // Discover the operator capability once (ADR-131): operators load + veto plugins
  // for any tenant without being a member of it.
  useEffect(() => {
    if (!client) return
    client.operatorStatus().then((s) => setIsOperator(s.is_operator)).catch(() => {})
  }, [client])

  const load = useCallback(async () => {
    if (!client || !tenant.trim()) return
    setLoading(true)
    setErr(null)
    try {
      // Operators read via the operator endpoint (no tenant membership needed);
      // tenant admins via the tenant endpoint.
      setPlugins(
        isOperator
          ? await client.listOperatorPlugins(tenant.trim())
          : await client.listTenantPlugins(tenant.trim()),
      )
    } catch (e) {
      setPlugins(null)
      setErr(
        e instanceof ApiError && e.status === 403
          ? `You need to be an admin of "${tenant.trim()}" to manage its plugins.`
          : e instanceof ApiError && e.status === 404
            ? `No tenant "${tenant.trim()}" — provision it on the Settings tab.`
            : 'Could not load plugins.',
      )
    } finally {
      setLoading(false)
    }
  }, [client, tenant, isOperator])

  useEffect(() => {
    void load()
  }, [load])

  // Surface a successful OAuth connect (the callback redirects to #connected=<kind>).
  useEffect(() => {
    const m = /[#&]connected=([\w-]+)/.exec(window.location.hash)
    if (m) {
      setNote(`Connected ${m[1]} — credentials captured.`)
      history.replaceState(null, '', window.location.pathname)
      void load()
    }
  }, [load])

  function setField(kind: string, name: string, value: string) {
    setDrafts((d) => ({ ...d, [kind]: { ...(d[kind] ?? {}), [name]: value } }))
  }

  async function save(p: TenantPlugin, enabled: boolean) {
    if (!client) return
    setErr(null)
    setNote(null)
    try {
      await client.setTenantPlugin(tenant.trim(), p.kind, {
        enabled,
        config: drafts[p.kind] ?? {},
      })
      setDrafts((d) => ({ ...d, [p.kind]: {} })) // clear typed secrets from memory
      await load()
    } catch (e) {
      setErr(e instanceof ApiError ? `Save failed (${e.status}): ${e.message.slice(0, 160)}` : 'Save failed.')
    }
  }

  async function operatorVeto(p: TenantPlugin, disabled: boolean) {
    if (!client) return
    setErr(null)
    setNote(null)
    try {
      await client.setOperatorPlugin(tenant.trim(), p.kind, disabled)
      setNote(
        disabled
          ? `Operator: disabled ${p.display_name} for "${tenant.trim()}".`
          : `Operator: re-allowed ${p.display_name} for "${tenant.trim()}".`,
      )
      await load()
    } catch (e) {
      setErr(e instanceof ApiError ? `Operator action failed (${e.status}).` : 'Operator action failed.')
    }
  }

  async function connect(p: TenantPlugin) {
    if (!client) return
    setErr(null)
    try {
      const { url } = await client.connectPlugin(tenant.trim(), p.kind)
      window.location.assign(url) // off to the provider's consent screen
    } catch (e) {
      setErr(
        e instanceof ApiError
          ? `Connect failed (${e.status}): ${e.message.slice(0, 160)}`
          : 'Connect failed.',
      )
    }
  }

  return (
    <div className="mx-auto max-w-2xl space-y-4">
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <h2 className="font-semibold text-slate-800">Delivery plugins</h2>
        <p className="mt-1 text-sm text-slate-500">
          Enable a plugin for your tenant and configure its account credentials once. Workspaces
          then choose the channel / space / folder on the <span className="font-medium">Delivery</span>{' '}
          tab. Secrets are stored encrypted and never shown again.
        </p>
        <div className="mt-3 flex items-center gap-2">
          <label className="text-sm font-medium text-slate-700">Tenant</label>
          <input
            value={tenant}
            onChange={(e) => setTenant(e.target.value)}
            className="rounded-lg border border-slate-300 px-3 py-1.5 text-sm outline-none focus:border-accent"
          />
          {loading && <span className="text-xs text-slate-400">loading…</span>}
        </div>
        {err && <p className="mt-3 text-sm text-red-600">{err}</p>}
        {note && <p className="mt-3 text-sm text-emerald-700">{note}</p>}

        {/* OAuth setup guidance — the #1 cause of provider "can't identify the
            app" errors is a missing client id or a redirect URI not registered. */}
        {connectInfo && (
          <div className="mt-3 rounded-lg bg-slate-50 px-3 py-2 text-xs text-slate-600">
            <p className="font-medium text-slate-700">Connecting via OAuth (Google Drive / Confluence)?</p>
            <p className="mt-1">
              On your OAuth app (Google Cloud console / Atlassian developer console), register this
              exact <span className="font-medium">redirect URL</span>:
            </p>
            <code className="mt-1 block break-all rounded bg-white px-2 py-1 ring-1 ring-slate-200">
              {connectInfo.uri}
            </code>
            <p className="mt-1">
              Server OAuth apps configured:{' '}
              {connectInfo.kinds.length ? (
                <span className="text-slate-700">{connectInfo.kinds.join(', ')}</span>
              ) : (
                <span className="text-amber-600">
                  none — set GOOGLE_CLIENT_ID/SECRET and/or ATLASSIAN_CLIENT_ID/SECRET on the server
                </span>
              )}
              . A "couldn't identify the app" error means the client id is wrong/blank or this
              redirect URL isn't on the app's allowed list.
            </p>
          </div>
        )}
      </div>

      {plugins?.length === 0 && (
        <div className="rounded-xl bg-white p-4 text-sm text-slate-500 shadow-sm ring-1 ring-slate-200">
          No delivery plugins are built into this server.
        </div>
      )}

      {plugins?.map((p) => (
        <PluginCard
          key={p.kind}
          plugin={p}
          draft={drafts[p.kind] ?? {}}
          isOperator={isOperator}
          onField={(n, v) => setField(p.kind, n, v)}
          onSave={(enabled) => void save(p, enabled)}
          onConnect={() => void connect(p)}
          onOperatorVeto={(disabled) => void operatorVeto(p, disabled)}
        />
      ))}
    </div>
  )
}

function Badge({ on, label, tone }: { on: boolean; label: string; tone: 'green' | 'slate' }) {
  const cls = on
    ? tone === 'green'
      ? 'bg-emerald-100 text-emerald-800'
      : 'bg-accent/15 text-accent'
    : 'bg-slate-100 text-slate-400'
  return <span className={`rounded px-1.5 py-0.5 text-[11px] font-medium ${cls}`}>{label}</span>
}

function PluginCard({
  plugin: p,
  draft,
  isOperator,
  onField,
  onSave,
  onConnect,
  onOperatorVeto,
}: {
  plugin: TenantPlugin
  draft: Record<string, string>
  isOperator: boolean
  onField: (name: string, value: string) => void
  onSave: (enabled: boolean) => void
  onConnect: () => void
  onOperatorVeto: (disabled: boolean) => void
}) {
  return (
    <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
      <div className="flex items-center justify-between">
        <h3 className="font-medium text-slate-800">{p.display_name}</h3>
        <div className="flex items-center gap-1.5">
          {p.operator_disabled && (
            <span className="rounded bg-red-100 px-1.5 py-0.5 text-[11px] font-medium text-red-700">
              disabled by operator
            </span>
          )}
          <Badge on={p.enabled} label={p.enabled ? 'enabled' : 'disabled'} tone="slate" />
          {p.tenant_fields.length > 0 && (
            <Badge on={p.configured} label={p.configured ? 'configured' : 'not configured'} tone="green" />
          )}
          {p.supports_connect && <Badge on={p.connected} label={p.connected ? 'connected' : 'not connected'} tone="green" />}
        </div>
      </div>

      {/* Tenant-admin advisory: the operator has turned this off for the tenant. */}
      {p.operator_disabled && !isOperator && (
        <p className="mt-2 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700">
          A platform operator has disabled this plugin for your tenant. It won't deliver until the
          operator re-enables it; your enable/disable setting has no effect meanwhile.
        </p>
      )}

      {/* Operator control (ADR-131): veto/allow this plugin for the tenant. */}
      {isOperator && (
        <div className="mt-2 flex items-center gap-2 rounded-lg bg-slate-50 px-3 py-2">
          <span className="text-xs font-medium uppercase tracking-wide text-slate-500">Operator</span>
          <button
            onClick={() => onOperatorVeto(!p.operator_disabled)}
            className={`rounded px-2.5 py-1 text-xs font-medium ${
              p.operator_disabled
                ? 'bg-emerald-600 text-white'
                : 'bg-red-600 text-white'
            }`}
          >
            {p.operator_disabled ? 'Re-allow for this tenant' : 'Disable for this tenant'}
          </button>
        </div>
      )}

      {p.hint && <p className="mt-1 text-xs text-slate-500">{p.hint}</p>}

      {/* Tenant credential fields (typed), unless this plugin connects via OAuth. */}
      {!p.supports_connect && p.tenant_fields.length > 0 && (
        <div className="mt-3 space-y-2">
          {p.tenant_fields.map((f) => (
            <div key={f.name}>
              <label className="block text-xs font-medium text-slate-600">{f.label}</label>
              <input
                type={f.secret ? 'password' : 'text'}
                value={draft[f.name] ?? ''}
                placeholder={f.secret && (p.configured || p.connected) ? '•••••• (unchanged)' : ''}
                onChange={(e) => onField(f.name, e.target.value)}
                className="mt-1 w-full rounded-lg border border-slate-300 px-3 py-1.5 text-sm outline-none focus:border-accent"
              />
            </div>
          ))}
        </div>
      )}

      {p.tenant_fields.length === 0 && !p.supports_connect && (
        <p className="mt-2 text-xs text-slate-400">
          No tenant credentials needed — configure the target per workspace on the Delivery tab.
        </p>
      )}

      {/* The workspace picks these per destination (shown for context). */}
      {p.workspace_fields.length > 0 && (
        <p className="mt-2 text-xs text-slate-400">
          Per-workspace target: {p.workspace_fields.map((f) => f.name).join(', ')}
        </p>
      )}

      <div className="mt-3 flex items-center gap-2">
        <button
          onClick={() => onSave(true)}
          disabled={p.operator_disabled && !isOperator}
          className="rounded-lg bg-accent px-3 py-1.5 text-sm font-medium text-accent-fg disabled:opacity-40"
        >
          {p.enabled ? 'Save' : 'Enable'}
        </button>
        {p.enabled && (
          <button
            onClick={() => onSave(false)}
            className="rounded-lg border border-slate-300 px-3 py-1.5 text-sm text-slate-600"
          >
            Disable
          </button>
        )}
        {p.supports_connect && (
          <button
            onClick={onConnect}
            className="rounded-lg border border-accent px-3 py-1.5 text-sm font-medium text-accent"
          >
            {p.connected ? 'Reconnect' : 'Connect'} {p.display_name}
          </button>
        )}
      </div>
    </div>
  )
}
