import { useState } from 'react'
import { useAuth } from '../auth'

export function Login() {
  const { signIn } = useAuth()
  const [workspace, setWorkspace] = useState('ws-demo')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)

  async function submit(e: React.FormEvent) {
    e.preventDefault()
    setBusy(true)
    setErr(null)
    try {
      await signIn(workspace)
    } catch {
      setErr('Sign-in failed — is the API running?')
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="flex min-h-full items-center justify-center p-6">
      <form
        onSubmit={submit}
        className="w-full max-w-sm rounded-2xl bg-white p-8 shadow-sm ring-1 ring-slate-200"
      >
        <h1 className="text-2xl font-semibold text-accent">SummaryBot</h1>
        <p className="mt-1 text-sm text-slate-500">Sign in to your workspace</p>

        <label className="mt-6 block text-sm font-medium text-slate-700">Workspace</label>
        <input
          value={workspace}
          onChange={(e) => setWorkspace(e.target.value)}
          className="mt-1 w-full rounded-lg border border-slate-300 px-3 py-2 outline-none focus:border-accent"
          placeholder="ws-demo"
        />

        {err && <p className="mt-3 text-sm text-red-600">{err}</p>}

        <button
          type="submit"
          disabled={busy}
          className="mt-6 w-full rounded-lg bg-accent px-4 py-2 font-medium text-accent-fg disabled:opacity-60"
        >
          {busy ? 'Signing in…' : 'Dev sign-in'}
        </button>

        <div className="mt-4 flex items-center gap-3 text-xs text-slate-400">
          <span className="h-px flex-1 bg-slate-200" />
          or
          <span className="h-px flex-1 bg-slate-200" />
        </div>

        {/* Real OAuth (ADR-126). Works when the server is built with
            --features oauth and the provider is configured; otherwise the
            start endpoint returns a clear error. */}
        <div className="mt-4 space-y-2">
          {(['google', 'discord'] as const).map((p) => (
            <a
              key={p}
              href={`/auth/oauth/${p}/start?workspaces=${encodeURIComponent(
                workspace.trim() || 'ws-demo',
              )}`}
              className="block w-full rounded-lg border border-slate-300 px-4 py-2 text-center text-sm font-medium capitalize text-slate-700 hover:border-accent"
            >
              Sign in with {p}
            </a>
          ))}
        </div>

        <p className="mt-4 text-center text-xs text-slate-400">
          Dev sign-in uses the email provider. OAuth needs the server built with{' '}
          <code className="rounded bg-slate-100 px-1">--features oauth</code> + provider keys.
        </p>
      </form>
    </div>
  )
}
