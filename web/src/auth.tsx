import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from 'react'
import { Client, loadSession, login as apiLogin, type Session } from './api'

/** Decode a `#session=<base64url-json>` fragment from the OAuth callback into a
 *  Session, or null if absent/malformed. Clears the fragment when consumed. */
function consumeOAuthFragment(): Session | null {
  const m = /[#&]session=([^&]+)/.exec(window.location.hash)
  if (!m) return null
  try {
    const b64 = m[1].replace(/-/g, '+').replace(/_/g, '/')
    const json = JSON.parse(atob(b64)) as Record<string, string>
    history.replaceState(null, '', window.location.pathname + window.location.search)
    if (!json.access_token || !json.refresh_token) return null
    return {
      accessToken: json.access_token,
      refreshToken: json.refresh_token,
      userId: json.user_id ?? '',
      workspaces: String(json.workspaces ?? '')
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean),
    }
  } catch {
    return null
  }
}

interface AuthCtx {
  client: Client | null
  signIn: (workspace: string) => Promise<void>
  signOut: () => void
}

const Ctx = createContext<AuthCtx | null>(null)

export function useAuth(): AuthCtx {
  const v = useContext(Ctx)
  if (!v) throw new Error('useAuth outside provider')
  return v
}

export function AuthProvider({ children }: { children: ReactNode }) {
  const [session, setSession] = useState<Session | null>(
    () => consumeOAuthFragment() ?? loadSession(),
  )

  // Catch an OAuth callback that lands after the initial render.
  useEffect(() => {
    const s = consumeOAuthFragment()
    if (s) setSession(s)
  }, [])

  const client = useMemo(
    () => (session ? new Client(session, () => setSession(null)) : null),
    [session],
  )

  async function signIn(workspace: string) {
    const ws = workspace.trim() || 'ws-demo'
    // Dev sign-in: email provider, a derived subject. Real OAuth is a later seam.
    const s = await apiLogin('email', `demo-${ws}`, 'demo@example.com', [ws])
    setSession(s)
  }

  function signOut() {
    client?.logout()
    setSession(null)
  }

  return <Ctx.Provider value={{ client, signIn, signOut }}>{children}</Ctx.Provider>
}
