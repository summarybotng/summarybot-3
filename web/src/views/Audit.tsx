import { useEffect, useState } from 'react'
import { useAuth } from '../auth'
import { ApiError } from '../api'
import type { AuditEntry } from '../types'

// Audit log (WSP-014). Security/admin events for this workspace's tenant —
// member role changes, invites, identity links — newest first. Admin-only; a
// 403 means the caller isn't an admin of the workspace's tenant, and an empty
// list is normal for an unprovisioned/dev workspace.
const ACTION_LABELS: Record<string, string> = {
  'member.role_set': 'Role changed',
  'member.removed': 'Member removed',
  'invite.issued': 'Invite issued',
  'identity.link': 'Identity linked',
  'identity.link.collision': 'Identity link rejected',
  'identity.provision': 'Account provisioned',
}

function when(ts: number): string {
  return new Date(ts * 1000).toLocaleString()
}

export function Audit() {
  const { client } = useAuth()
  const [entries, setEntries] = useState<AuditEntry[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    if (!client) return
    setLoading(true)
    client
      .listAudit(100)
      .then((e) => {
        setEntries(e)
        setError(null)
      })
      .catch((e) => {
        setEntries(null)
        setError(
          e instanceof ApiError && e.status === 403
            ? 'You need an admin role on this workspace’s tenant to view the audit log.'
            : 'Could not load the audit log.',
        )
      })
      .finally(() => setLoading(false))
  }, [client])

  return (
    <div className="mx-auto max-w-2xl space-y-4">
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <h2 className="font-semibold text-slate-800">Audit log</h2>
        <p className="mt-1 text-sm text-slate-500">
          Security and admin events — role changes, invites, identity links — newest first.
        </p>

        {loading ? (
          <p className="mt-4 text-sm text-slate-400">Loading…</p>
        ) : error ? (
          <p className="mt-4 text-sm text-slate-500">{error}</p>
        ) : entries && entries.length > 0 ? (
          <ul className="mt-4 divide-y divide-slate-100">
            {entries.map((e, i) => (
              <li key={i} className="py-2.5">
                <div className="flex items-baseline justify-between gap-3">
                  <span className="text-sm font-medium text-slate-800">
                    {ACTION_LABELS[e.action] ?? e.action}
                  </span>
                  <span className="shrink-0 text-xs text-slate-400">{when(e.ts)}</span>
                </div>
                <div className="mt-0.5 text-sm text-slate-600">{e.detail}</div>
                <div className="mt-0.5 text-xs text-slate-400">
                  <code className="rounded bg-slate-100 px-1">{e.action}</code>
                  {e.actor && <span className="ml-2">by {e.actor}</span>}
                </div>
              </li>
            ))}
          </ul>
        ) : (
          <p className="mt-4 text-sm text-slate-400">
            No audit events yet — they appear here as members, roles, and invites change.
          </p>
        )}
      </div>
    </div>
  )
}
