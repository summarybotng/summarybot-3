import { useEffect, useState } from 'react'
import { useAuth } from './auth'
import { applyTenantAccent } from './theme'
import { Login } from './views/Login'
import { Summaries } from './views/Summaries'
import { Schedules } from './views/Schedules'
import { Settings } from './views/Settings'
import { Whatsapp } from './views/Whatsapp'
import { Delivery } from './views/Delivery'
import { Knowledge } from './views/Knowledge'
import { Source } from './views/Source'
import { Spend } from './views/Spend'
import { Audit } from './views/Audit'
import { Members } from './views/Members'
import { Plugins } from './views/Plugins'

type Tab =
  | 'summaries'
  | 'schedules'
  | 'whatsapp'
  | 'discord'
  | 'slack'
  | 'delivery'
  | 'plugins'
  | 'knowledge'
  | 'spend'
  | 'members'
  | 'audit'
  | 'settings'

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
    <div className="flex h-screen overflow-hidden">
      <aside className="flex w-52 shrink-0 flex-col border-r border-slate-200 bg-white">
        <div className="flex items-center gap-2 border-b border-slate-100 px-4 py-4">
          <span className="h-3 w-3 shrink-0 rounded-full bg-accent" />
          <div className="min-w-0">
            <div className="truncate font-semibold text-slate-800">{brand}</div>
            <div className="truncate text-xs text-slate-400">{client.ws()}</div>
          </div>
        </div>

        <nav className="flex-1 space-y-4 overflow-y-auto px-2 py-3">
          {NAV.map((group, gi) => (
            <div key={gi}>
              {group.section && (
                <div className="px-2 pb-1 text-xs font-medium uppercase tracking-wide text-slate-400">
                  {group.section}
                </div>
              )}
              <div className="space-y-0.5">
                {group.tabs.map((t) => (
                  <button
                    key={t}
                    onClick={() => setTab(t)}
                    className={`block w-full rounded-lg px-3 py-1.5 text-left text-sm capitalize ${
                      tab === t
                        ? 'bg-accent/10 font-medium text-accent'
                        : 'text-slate-600 hover:bg-slate-100'
                    }`}
                  >
                    {t}
                  </button>
                ))}
              </div>
            </div>
          ))}
        </nav>

        <button
          onClick={signOut}
          className="border-t border-slate-100 px-4 py-3 text-left text-sm text-slate-500 hover:text-slate-800"
        >
          Sign out
        </button>
      </aside>

      <main className="flex-1 overflow-y-auto px-4 py-6">
        {tab === 'summaries' && <Summaries />}
        {tab === 'schedules' && <Schedules />}
        {tab === 'whatsapp' && <Whatsapp />}
        {tab === 'discord' && <Source platform="discord" />}
        {tab === 'slack' && <Source platform="slack" />}
        {tab === 'delivery' && <Delivery />}
        {tab === 'plugins' && <Plugins />}
        {tab === 'knowledge' && <Knowledge />}
        {tab === 'spend' && <Spend />}
        {tab === 'members' && <Members />}
        {tab === 'audit' && <Audit />}
        {tab === 'settings' && <Settings />}
      </main>
    </div>
  )
}

/// Left-nav groups (vertical sidebar). Grouping keeps 11 tabs scannable.
const NAV: { section?: string; tabs: Tab[] }[] = [
  { tabs: ['summaries', 'schedules', 'knowledge', 'spend'] },
  { section: 'Sources', tabs: ['whatsapp', 'discord', 'slack'] },
  { section: 'Admin', tabs: ['delivery', 'plugins', 'members', 'audit', 'settings'] },
]
