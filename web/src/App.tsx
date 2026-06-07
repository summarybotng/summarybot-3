import { useEffect, useState } from 'react'
import { useAuth } from './auth'
import { applyTenantAccent } from './theme'
import { Login } from './views/Login'
import { Summaries } from './views/Summaries'
import { Schedules } from './views/Schedules'
import { Settings } from './views/Settings'

type Tab = 'summaries' | 'schedules' | 'settings'

export default function App() {
  const { client, signOut } = useAuth()
  const [tab, setTab] = useState<Tab>('summaries')
  const [brand, setBrand] = useState<string>('SummaryBot')

  // Resolve the tenant for branding (name + derived accent). In local dev the
  // Host won't match a tenant, so this quietly falls back to defaults.
  useEffect(() => {
    if (!client) {
      applyTenantAccent(null)
      setBrand('SummaryBot')
      return
    }
    client
      .tenant()
      .then((t) => {
        applyTenantAccent(t.id)
        setBrand(t.name)
      })
      .catch(() => {
        applyTenantAccent(null)
        setBrand('SummaryBot')
      })
  }, [client])

  if (!client) return <Login />

  return (
    <div className="flex min-h-full flex-col">
      <header className="sticky top-0 z-10 border-b border-slate-200 bg-white/90 backdrop-blur">
        <div className="mx-auto flex max-w-3xl items-center justify-between px-4 py-3">
          <div className="flex items-center gap-2">
            <span className="h-3 w-3 rounded-full bg-accent" />
            <span className="font-semibold text-slate-800">{brand}</span>
            <span className="hidden text-xs text-slate-400 sm:inline">/ {client.ws()}</span>
          </div>
          <button onClick={signOut} className="text-sm text-slate-500 hover:text-slate-800">
            Sign out
          </button>
        </div>
        <nav className="mx-auto flex max-w-3xl gap-1 px-4">
          {(['summaries', 'schedules', 'settings'] as Tab[]).map((t) => (
            <button
              key={t}
              onClick={() => setTab(t)}
              className={`-mb-px border-b-2 px-3 py-2 text-sm font-medium capitalize ${
                tab === t
                  ? 'border-accent text-accent'
                  : 'border-transparent text-slate-500 hover:text-slate-800'
              }`}
            >
              {t}
            </button>
          ))}
        </nav>
      </header>

      <main className="flex-1 px-4 py-6">
        {tab === 'summaries' && <Summaries />}
        {tab === 'schedules' && <Schedules />}
        {tab === 'settings' && <Settings />}
      </main>
    </div>
  )
}
