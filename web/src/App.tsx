import { useEffect, useState } from 'react'
import { useAuth } from './auth'
import { applyTenantAccent } from './theme'
import { Login } from './views/Login'
import { Summaries } from './views/Summaries'
import { CreateSummary } from './views/CreateSummary'
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
import { Jobs } from './views/Jobs'
import { Coverage } from './views/Coverage'
import { Prompts } from './views/Prompts'
import { Errors } from './views/Errors'

type Tab =
  | 'create'
  | 'summaries'
  | 'schedules'
  | 'whatsapp'
  | 'discord'
  | 'slack'
  | 'delivery'
  | 'plugins'
  | 'knowledge'
  | 'coverage'
  | 'prompts'
  | 'spend'
  | 'jobs'
  | 'members'
  | 'audit'
  | 'errors'
  | 'settings'

export default function App() {
  const { client, signOut } = useAuth()
  const [tab, setTab] = useState<Tab>('summaries')
  const [brand, setBrand] = useState<string>('SummaryBot')
  // Active workspace (one of the session's granted set). Switching it remounts
  // the content so every tab refetches for the new workspace.
  const [activeWs, setActiveWs] = useState('')
  useEffect(() => {
    if (client) setActiveWs(client.ws())
  }, [client])

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
          <div className="min-w-0 flex-1">
            <div className="truncate font-semibold text-slate-800">{brand}</div>
            {client.workspaces().length > 1 ? (
              <select
                value={activeWs}
                onChange={(e) => {
                  client.setActiveWorkspace(e.target.value)
                  setActiveWs(e.target.value)
                }}
                title="Switch workspace"
                className="mt-0.5 w-full truncate bg-transparent text-xs text-slate-500 outline-none"
              >
                {client.workspaces().map((w) => (
                  <option key={w} value={w}>
                    {w}
                  </option>
                ))}
              </select>
            ) : (
              <div className="truncate text-xs text-slate-400">{client.ws()}</div>
            )}
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

      <main key={activeWs} className="flex-1 overflow-y-auto px-4 py-6">
        {tab === 'create' && <CreateSummary />}
        {tab === 'summaries' && <Summaries />}
        {tab === 'schedules' && <Schedules />}
        {tab === 'whatsapp' && <Whatsapp />}
        {tab === 'discord' && <Source platform="discord" />}
        {tab === 'slack' && <Source platform="slack" />}
        {tab === 'delivery' && <Delivery />}
        {tab === 'plugins' && <Plugins />}
        {tab === 'jobs' && <Jobs />}
        {tab === 'coverage' && <Coverage />}
        {tab === 'prompts' && <Prompts />}
        {tab === 'errors' && <Errors />}
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
  { tabs: ['create', 'summaries', 'schedules', 'knowledge', 'coverage', 'spend', 'jobs'] },
  { section: 'Sources', tabs: ['whatsapp', 'discord', 'slack'] },
  { section: 'Admin', tabs: ['delivery', 'plugins', 'prompts', 'members', 'audit', 'errors', 'settings'] },
]
