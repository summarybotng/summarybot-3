import { useCallback, useEffect, useState } from 'react'
import { useAuth } from '../auth'
import { ApiError } from '../api'
import type { Invite, Membership } from '../types'

// Tenant members + invites (RBAC admin, TEN-005). The dashboard is workspace-
// scoped, so the tenant is chosen by id here (you become Owner by provisioning
// one on the Settings tab, or claim an existing one you're a member of). All
// actions require Admin/Owner on that tenant; a 403 means insufficient role.
const ROLES = ['member', 'admin', 'owner']

function when(ts: number): string {
  return new Date(ts * 1000).toLocaleDateString()
}

export function Members() {
  const { client } = useAuth()
  const [tenant, setTenant] = useState('acme')
  const [members, setMembers] = useState<Membership[] | null>(null)
  const [invites, setInvites] = useState<Invite[]>([])
  const [err, setErr] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)

  // Invite form.
  const [email, setEmail] = useState('')
  const [inviteRole, setInviteRole] = useState('member')
  const [issued, setIssued] = useState<string | null>(null)

  const load = useCallback(async () => {
    if (!client || !tenant.trim()) return
    setLoading(true)
    setErr(null)
    try {
      const [m, i] = await Promise.all([
        client.listMembers(tenant.trim()),
        client.listInvites(tenant.trim()).catch(() => [] as Invite[]),
      ])
      setMembers(m)
      setInvites(i)
    } catch (e) {
      setMembers(null)
      setInvites([])
      setErr(
        e instanceof ApiError && e.status === 403
          ? `You need to be a member of "${tenant.trim()}" to view it.`
          : e instanceof ApiError && e.status === 404
            ? `No tenant "${tenant.trim()}" — provision it on the Settings tab.`
            : 'Could not load the tenant.',
      )
    } finally {
      setLoading(false)
    }
  }, [client, tenant])

  useEffect(() => {
    void load()
  }, [load])

  async function act(p: Promise<unknown>) {
    setErr(null)
    try {
      await p
      await load()
    } catch (e) {
      setErr(e instanceof ApiError ? `Action failed (${e.status})` : 'Action failed.')
    }
  }

  async function invite(e: React.FormEvent) {
    e.preventDefault()
    if (!client || !email.trim()) return
    setErr(null)
    try {
      const r = await client.createInvite(tenant.trim(), email.trim(), inviteRole)
      setIssued(r.token) // shown once
      setEmail('')
      await load()
    } catch (e) {
      setErr(e instanceof ApiError ? `Could not issue invite (${e.status})` : 'Could not issue invite.')
    }
  }

  return (
    <div className="mx-auto max-w-2xl space-y-4">
      {/* Tenant selector */}
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <h2 className="font-semibold text-slate-800">Tenant members</h2>
        <p className="mt-1 text-sm text-slate-500">
          Manage who belongs to a tenant and their role. Provision a new tenant on the Settings tab.
        </p>
        <div className="mt-3 flex gap-2">
          <input
            value={tenant}
            onChange={(e) => setTenant(e.target.value)}
            placeholder="tenant id"
            className="flex-1 rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
          />
          <button
            onClick={() => void load()}
            className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-fg"
          >
            Load
          </button>
        </div>
        {err && <p className="mt-3 text-sm text-slate-600">{err}</p>}
      </div>

      {/* Members */}
      {members && (
        <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
          <p className="text-sm font-medium text-slate-700">
            Members {loading && <span className="text-slate-400">· loading…</span>}
          </p>
          {members.length === 0 ? (
            <p className="mt-2 text-sm text-slate-400">No members.</p>
          ) : (
            <ul className="mt-2 divide-y divide-slate-100">
              {members.map((m) => (
                <li key={m.user_id} className="flex items-center justify-between gap-3 py-2">
                  <span className="min-w-0 flex-1 truncate font-mono text-sm text-slate-700">
                    {m.user_id}
                  </span>
                  <select
                    value={m.role}
                    onChange={(e) => void act(client!.setMemberRole(tenant.trim(), m.user_id, e.target.value))}
                    className="rounded-lg border border-slate-300 px-2 py-1 text-sm"
                  >
                    {ROLES.map((r) => (
                      <option key={r} value={r}>
                        {r}
                      </option>
                    ))}
                  </select>
                  <button
                    title="Remove member"
                    onClick={() => void act(client!.removeMember(tenant.trim(), m.user_id))}
                    className="rounded px-2 py-1 text-sm hover:bg-red-50"
                  >
                    🗑
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      {/* Invites */}
      {members && (
        <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
          <p className="text-sm font-medium text-slate-700">Invites</p>
          <form onSubmit={invite} className="mt-2 flex gap-2">
            <input
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              placeholder="invitee email"
              className="flex-1 rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
            />
            <select
              value={inviteRole}
              onChange={(e) => setInviteRole(e.target.value)}
              className="rounded-lg border border-slate-300 px-2 py-2 text-sm"
            >
              {ROLES.map((r) => (
                <option key={r} value={r}>
                  {r}
                </option>
              ))}
            </select>
            <button
              type="submit"
              disabled={!email.trim()}
              className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-fg disabled:opacity-50"
            >
              Invite
            </button>
          </form>

          {issued && (
            <div className="mt-3 rounded-lg bg-amber-50 p-3 text-sm ring-1 ring-amber-200">
              <p className="font-medium text-amber-800">Invite token (shown once — copy it now):</p>
              <code className="mt-1 block break-all rounded bg-white px-2 py-1 text-xs text-slate-700">
                {issued}
              </code>
              <button
                onClick={() => setIssued(null)}
                className="mt-2 text-xs text-amber-700 underline"
              >
                dismiss
              </button>
            </div>
          )}

          {invites.length > 0 && (
            <ul className="mt-3 divide-y divide-slate-100">
              {invites.map((i) => (
                <li key={i.token_hash} className="flex items-center justify-between gap-3 py-2 text-sm">
                  <span className="min-w-0 flex-1 truncate text-slate-700">
                    {i.email}{' '}
                    <span className="text-slate-400">
                      · {i.role} · {i.status} · expires {when(i.expires_at)}
                    </span>
                  </span>
                  {i.status === 'pending' && (
                    <button
                      onClick={() => void act(client!.revokeInvite(tenant.trim(), i.token_hash))}
                      className="rounded border border-slate-300 px-2 py-1 text-xs"
                    >
                      Revoke
                    </button>
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  )
}
