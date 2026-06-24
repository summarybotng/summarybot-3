import { useCallback, useEffect, useState } from 'react'
import { useAuth } from '../auth'
import type { ChatCoverage, Prompts, SourceChannel, SourceServer, Summary } from '../types'

// Unified "Create Summary" wizard (ADR-088/089): one entry point for all three
// summary-creation flows — Now / Recurring / Past — that previously lived as
// separate controls on the Summaries, Schedules and WhatsApp tabs. Step 1 picks
// *what* (platform + channel/chat), step 2 picks *when*, and dispatches to the
// existing, tested endpoints (summarize-now, createSchedule, summarize-weeks).

type Platform = 'whatsapp' | 'discord' | 'slack'
type When = 'now' | 'recurring' | 'past'

const PLATFORMS: { id: Platform; icon: string; label: string; unit: string }[] = [
  { id: 'whatsapp', icon: '📱', label: 'WhatsApp', unit: 'chats' },
  { id: 'discord', icon: '🎮', label: 'Discord', unit: 'channels' },
  { id: 'slack', icon: '💬', label: 'Slack', unit: 'channels' },
]

const NOW_PRESETS: { label: string; secs: number }[] = [
  { label: 'last 4h', secs: 4 * 3600 },
  { label: 'last 24h', secs: 24 * 3600 },
  { label: 'last 7d', secs: 7 * 86400 },
  { label: 'last 30d', secs: 30 * 86400 },
]

const ALL = '*'

const LENGTHS: { id: string; label: string }[] = [
  { id: 'brief', label: 'Brief' },
  { id: 'detailed', label: 'Detailed' },
  { id: 'comprehensive', label: 'Comprehensive' },
]

// Resolve the combined perspective/template picker value into the two server
// fields. `t:<id>` → a saved template; `p:<id>` → a built-in perspective
// (`general` means "no steering"); '' → default.
function resolveSteer(v: string): { perspective: string | null; prompt_template_id: string | null } {
  if (v.startsWith('t:')) return { perspective: null, prompt_template_id: v.slice(2) }
  if (v.startsWith('p:')) {
    const id = v.slice(2)
    return { perspective: id === 'general' ? null : id, prompt_template_id: null }
  }
  return { perspective: null, prompt_template_id: null }
}

export function CreateSummary() {
  const { client } = useAuth()
  const [step, setStep] = useState<1 | 2>(1)
  const [platform, setPlatform] = useState<Platform | null>(null)

  // Steering options (ADR-133 §B / v2 parity): perspective/template + length.
  const [prompts, setPrompts] = useState<Prompts | null>(null)
  const [steer, setSteer] = useState('') // '' | p:<id> | t:<id>
  const [length, setLength] = useState('detailed')

  // Channel/chat selection.
  const [chats, setChats] = useState<ChatCoverage[]>([])
  const [servers, setServers] = useState<SourceServer[]>([])
  const [server, setServer] = useState('')
  const [channels, setChannels] = useState<SourceChannel[]>([])
  const [channel, setChannel] = useState('') // '' = none picked, '*' = all
  const [loadErr, setLoadErr] = useState<string | null>(null)

  // Step 2 — when.
  const [when, setWhen] = useState<When>('now')
  const [busy, setBusy] = useState(false)
  const [result, setResult] = useState<string | null>(null)
  const [made, setMade] = useState<Summary | null>(null)

  // Load perspectives + saved templates once for the options picker.
  useEffect(() => {
    if (!client) return
    client.listPrompts().then(setPrompts).catch(() => setPrompts(null))
  }, [client])

  // Reset downstream state whenever the platform changes.
  useEffect(() => {
    setServer('')
    setChannel('')
    setChannels([])
    setServers([])
    setChats([])
    setLoadErr(null)
    if (!client || !platform) return
    if (platform === 'whatsapp') {
      client.whatsappChats().then(setChats).catch(() => setLoadErr('Could not load chats.'))
    } else {
      client
        .sourceServers(platform)
        .then(setServers)
        .catch(() => setLoadErr(`Connect a ${platform} bot token on its tab first.`))
    }
  }, [client, platform])

  const loadChannels = useCallback(
    async (scopeId: string) => {
      if (!client || !platform) return
      setServer(scopeId)
      setChannel('')
      try {
        setChannels(await client.sourceChannels(platform, scopeId))
      } catch {
        setLoadErr('Could not load channels.')
      }
    },
    [client, platform],
  )

  async function submit() {
    if (!client || !channel) return
    setBusy(true)
    setResult(null)
    setMade(null)
    try {
      const { perspective, prompt_template_id } = resolveSteer(steer)
      if (when === 'now') {
        const src = platform === 'whatsapp' ? undefined : { platform: platform as string, sourceId: server || null }
        const r = await client.summarizeChannelNow(channel, nowSecs, src, {
          perspective,
          prompt_template_id,
          length,
        })
        setMade(r.summary)
        setResult(
          r.produced ? 'Summary produced — see it below / on the Summaries tab.' : 'No messages in that window — nothing produced.',
        )
      } else if (when === 'recurring') {
        await client.createSchedule({
          schedule_type: freq,
          hour,
          minute: 0,
          timezone: tz,
          channel,
          platform: platform === 'whatsapp' ? null : platform,
          source_id: platform === 'whatsapp' ? null : server || null,
          rolling_period: rolling || null,
          perspective,
          prompt_template_id,
          length,
        })
        setResult('Schedule created — manage it on the Schedules tab.')
      } else {
        // Past → by-week retrospective (WhatsApp imported history).
        const r = await client.summarizeWeeks(channel)
        setResult(`Retrospective done — ${r.produced} weekly ${r.produced === 1 ? 'summary' : 'summaries'} created.`)
      }
    } catch (e) {
      setResult('Failed: ' + (e instanceof Error ? e.message : 'unknown error'))
    } finally {
      setBusy(false)
    }
  }

  // Step-2 form state.
  const [nowSecs, setNowSecs] = useState(24 * 3600)
  const [freq, setFreq] = useState('daily')
  const [hour, setHour] = useState(9)
  const [tz, setTz] = useState('UTC')
  const [rolling, setRolling] = useState('')

  const canPast = platform === 'whatsapp' && channel !== ALL && channel !== ''

  // Distinct categories among the loaded channels (Discord), for a category scope.
  const categories = Array.from(
    new Map(
      channels
        .filter((c) => c.category_id && c.category)
        .map((c) => [c.category_id as string, { id: c.category_id as string, name: c.category as string }]),
    ).values(),
  )

  return (
    <div className="mx-auto max-w-2xl space-y-6">
      <div className="flex items-center gap-2 text-sm text-slate-500">
        <Stepper n={1} cur={step} label="What" />
        <span className="h-px w-6 bg-slate-300" />
        <Stepper n={2} cur={step} label="When" />
      </div>

      {step === 1 && (
        <div className="space-y-4 rounded-xl bg-white p-5 shadow-sm ring-1 ring-slate-200">
          <p className="text-sm font-medium text-slate-700">What would you like to summarize?</p>
          <div className="grid grid-cols-3 gap-3">
            {PLATFORMS.map((p) => (
              <button
                key={p.id}
                onClick={() => setPlatform(p.id)}
                className={`flex flex-col items-center gap-1 rounded-xl border p-4 text-sm ${
                  platform === p.id ? 'border-accent bg-accent/5 text-accent' : 'border-slate-200 text-slate-600'
                }`}
              >
                <span className="text-2xl">{p.icon}</span>
                {p.label}
                <span className="text-xs text-slate-400">{p.unit}</span>
              </button>
            ))}
          </div>

          {loadErr && <p className="text-sm text-amber-600">{loadErr}</p>}

          {/* Discord/Slack: pick a server first, then a channel. */}
          {platform && platform !== 'whatsapp' && servers.length > 0 && (
            <label className="block text-sm">
              <span className="text-slate-500">Server</span>
              <select
                value={server}
                onChange={(e) => void loadChannels(e.target.value)}
                className="mt-1 w-full rounded-lg border border-slate-300 px-2 py-2 text-sm"
              >
                <option value="">choose a server…</option>
                {servers.map((s) => (
                  <option key={s.id} value={s.id}>
                    {s.name}
                  </option>
                ))}
              </select>
            </label>
          )}

          {/* Channel / chat selection (with "all channels" + per-category shortcuts). */}
          {platform && (
            <div className="space-y-1">
              <p className="text-sm text-slate-500">Select {platform === 'whatsapp' ? 'a chat' : 'a channel'}</p>
              <label className="flex items-center gap-2 text-sm">
                <input type="radio" checked={channel === ALL} onChange={() => setChannel(ALL)} />
                <span>All {platform === 'whatsapp' ? 'chats' : 'channels'} in this workspace</span>
              </label>
              {/* Category scope (ADR-011): one option per distinct Discord category. */}
              {categories.map((cat) => {
                const val = `category:${cat.id}`
                return (
                  <label key={cat.id} className="flex items-center gap-2 text-sm">
                    <input type="radio" checked={channel === val} onChange={() => setChannel(val)} />
                    <span>📂 All channels in <span className="font-medium">{cat.name}</span></span>
                  </label>
                )
              })}
              {(platform === 'whatsapp' ? chats.map((c) => ({ id: c.chat_id, name: c.chat_id })) : channels).map(
                (c) => (
                  <label key={c.id} className="flex items-center gap-2 text-sm">
                    <input type="radio" checked={channel === c.id} onChange={() => setChannel(c.id)} />
                    <span className="truncate">
                      {platform !== 'whatsapp' && 'category' in c && c.category ? `${c.category} / ` : ''}
                      {c.name}
                    </span>
                  </label>
                ),
              )}
            </div>
          )}

          <div className="flex justify-end">
            <button
              disabled={!channel}
              onClick={() => setStep(2)}
              className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-fg disabled:opacity-50"
            >
              Next →
            </button>
          </div>
        </div>
      )}

      {step === 2 && (
        <div className="space-y-4 rounded-xl bg-white p-5 shadow-sm ring-1 ring-slate-200">
          <p className="text-sm font-medium text-slate-700">When do you want this summary?</p>
          <div className="grid grid-cols-3 gap-3">
            {(
              [
                { id: 'now', icon: '⚡', label: 'Now', sub: 'recent messages' },
                { id: 'recurring', icon: '🔄', label: 'Recurring', sub: 'schedule it' },
                { id: 'past', icon: '📅', label: 'Past', sub: 'by week' },
              ] as { id: When; icon: string; label: string; sub: string }[]
            ).map((w) => (
              <button
                key={w.id}
                onClick={() => setWhen(w.id)}
                disabled={w.id === 'past' && !canPast}
                className={`flex flex-col items-center gap-1 rounded-xl border p-4 text-sm disabled:opacity-40 ${
                  when === w.id ? 'border-accent bg-accent/5 text-accent' : 'border-slate-200 text-slate-600'
                }`}
                title={w.id === 'past' && !canPast ? 'Retrospective is available per WhatsApp chat' : ''}
              >
                <span className="text-2xl">{w.icon}</span>
                {w.label}
                <span className="text-xs text-slate-400">{w.sub}</span>
              </button>
            ))}
          </div>

          {when === 'now' && (
            <div className="flex flex-wrap gap-2">
              {NOW_PRESETS.map((p) => (
                <button
                  key={p.secs}
                  onClick={() => setNowSecs(p.secs)}
                  className={`rounded-full px-3 py-1 text-xs ring-1 ${
                    nowSecs === p.secs ? 'bg-accent text-accent-fg ring-accent' : 'bg-white text-slate-600 ring-slate-300'
                  }`}
                >
                  {p.label}
                </button>
              ))}
            </div>
          )}

          {when === 'recurring' && (
            <div className="flex flex-wrap items-center gap-2">
              <select value={freq} onChange={(e) => setFreq(e.target.value)} className="rounded-lg border border-slate-300 px-2 py-2 text-sm">
                <option value="hourly">Hourly</option>
                <option value="daily">Daily</option>
                <option value="weekly">Weekly</option>
                <option value="monthly">Monthly</option>
              </select>
              <input
                type="number"
                min={0}
                max={23}
                value={hour}
                onChange={(e) => setHour(Number(e.target.value))}
                disabled={freq === 'hourly'}
                className="w-20 rounded-lg border border-slate-300 px-2 py-2 text-sm disabled:bg-slate-100"
                placeholder="hour"
              />
              <input value={tz} onChange={(e) => setTz(e.target.value)} className="w-28 rounded-lg border border-slate-300 px-2 py-2 text-sm" placeholder="UTC" />
              <select value={rolling} onChange={(e) => setRolling(e.target.value)} className="rounded-lg border border-slate-300 px-2 py-2 text-sm">
                <option value="">one summary per run</option>
                <option value="weekly">rolling weekly</option>
                <option value="biweekly">rolling biweekly</option>
                <option value="monthly">rolling monthly</option>
              </select>
            </div>
          )}

          {when === 'past' && (
            <p className="text-sm text-slate-500">
              Produces one summary per non-empty week across this chat's imported history.
            </p>
          )}

          {/* Summary options (ADR-133 §B / v2 parity): perspective/template + length.
              Retrospective (Past) uses workspace defaults, so hide it there. */}
          {when !== 'past' && (
            <div className="flex flex-wrap items-end gap-3 border-t border-slate-100 pt-3">
              <label className="flex-1 text-sm">
                <span className="text-slate-500">Perspective / template</span>
                <select
                  value={steer}
                  onChange={(e) => setSteer(e.target.value)}
                  className="mt-1 w-full rounded-lg border border-slate-300 px-2 py-2 text-sm"
                >
                  <option value="">General (default)</option>
                  {(prompts?.perspectives ?? [])
                    .filter((p) => p.id !== 'general')
                    .map((p) => (
                      <option key={p.id} value={`p:${p.id}`}>
                        {p.label}
                      </option>
                    ))}
                  {(prompts?.templates ?? []).map((t) => (
                    <option key={t.id} value={`t:${t.id}`}>
                      📝 {t.name}
                    </option>
                  ))}
                </select>
              </label>
              <label className="text-sm">
                <span className="text-slate-500">Length</span>
                <select
                  value={length}
                  onChange={(e) => setLength(e.target.value)}
                  className="mt-1 rounded-lg border border-slate-300 px-2 py-2 text-sm"
                >
                  {LENGTHS.map((l) => (
                    <option key={l.id} value={l.id}>
                      {l.label}
                    </option>
                  ))}
                </select>
              </label>
            </div>
          )}

          <div className="flex items-center justify-between">
            <button onClick={() => setStep(1)} className="rounded-lg border border-slate-300 px-4 py-2 text-sm">
              ← Back
            </button>
            <button
              disabled={busy}
              onClick={() => void submit()}
              className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-fg disabled:opacity-50"
            >
              {busy ? 'Working…' : when === 'recurring' ? 'Create schedule' : 'Generate'}
            </button>
          </div>

          {result && <p className="rounded-lg bg-slate-50 px-3 py-2 text-sm text-slate-700">{result}</p>}
          {made && (
            <div className="rounded-lg border border-slate-200 p-3">
              <p className="text-sm font-medium text-slate-800">{made.text.split('\n')[0]}</p>
              {made.key_points.length > 0 && (
                <ul className="mt-1 list-disc pl-5 text-xs text-slate-600">
                  {made.key_points.slice(0, 5).map((k, i) => (
                    <li key={i}>{k.text}</li>
                  ))}
                </ul>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  )
}

function Stepper({ n, cur, label }: { n: 1 | 2; cur: number; label: string }) {
  const active = cur >= n
  return (
    <span className={`flex items-center gap-1.5 ${active ? 'text-accent' : ''}`}>
      <span className={`flex h-5 w-5 items-center justify-center rounded-full text-xs ${active ? 'bg-accent text-accent-fg' : 'bg-slate-200'}`}>
        {n}
      </span>
      {label}
    </span>
  )
}
