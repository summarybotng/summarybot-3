import { createContext, useContext, useMemo, useState, type ReactNode } from 'react'
import { Client, loadSession, login as apiLogin, type Session } from './api'

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
  const [session, setSession] = useState<Session | null>(() => loadSession())

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
