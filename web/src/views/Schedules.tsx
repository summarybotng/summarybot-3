import { useCallback, useEffect, useState } from 'react'
import { useAuth } from '../auth'
import type { Destination, Schedule, ScheduleRun } from '../types'

function when(ts: number): string {
  return new Date(ts * 1000).toLocaleString()
}

export function Schedules() {
  const { client } = useAuth()
  const [items, setItems] = useState<Schedule[]>([])
  const [runs, setRuns] = useState<Record<string, ScheduleRun[]>>({})
  const [type, setType] = useState('daily')
  const [hour, setHour] = useState(9)
  const [minute, setMinute] = useState(0)
  const [tz, setTz] = useState('UTC')
  const [channel, setChannel] = useState('')
  const [allChannels, setAllChannels] = useState(false)
  const [platform, setPlatform] = useState('')
  const [sourceId, setSourceId] = useState('')
  const [rollingPeriod, setRollingPeriod] = useState('')
  const [dests, setDests] = useState<Destination[]>([])
  const [pinned, setPinned] = useState<string[]>([])
  const [note, setNote] = useState<string | null>(null)
  // Steering options (ADR-133 §B).
  const [perspective, setPerspective] = useState('')
  const [templateId, setTemplateId] = useState('')
  const [titleTemplate, setTitleTemplate] = useState('')
  const [continuity, setContinuity] = useState(false)
  const [perspectives, setPerspectives] = useState<{ id: string; label: string }[]>([])
  const [templates, setTemplates] = useState<{ id: string; name: string }[]>([])

  const load = useCallback(async () => {
    if (!client) return
    setItems(await client.listSchedules())
    setDests((await client.listDestinations()).filter((d) => d.enabled))
    try {
      const p = await client.listPrompts()
      setPerspectives(p.perspectives)
      setTemplates(p.templates.map((t) => ({ id: t.id, name: t.name })))
    } catch {
      /* prompts optional */
    }
  }, [client])

  useEffect(() => {
    void load()
  }, [load])

  async function create(e: React.FormEvent) {
    e.preventDefault()
    if (!client) return
    await client.createSchedule({
      schedule_type: type,
      hour,
      minute,
      timezone: tz,
      // Scope (ADR-011): all of the workspace's channels, or one specific channel.
      channel: allChannels ? '*' : channel.trim() || null,
      platform: platform || null,
      source_id: sourceId.trim() || null,
      rolling_period: rollingPeriod || null,
      // Delivery scope (ADR-014): empty = all enabled destinations.
      destinations: pinned,
      // Steering (ADR-133 §B).
      perspective: templateId ? null : perspective || null,
      prompt_template_id: templateId || null,
      title_template: titleTemplate.trim() || null,
      enable_continuity: continuity,
    })
    setChannel('')
    setSourceId('')
    setPinned([])
    setTitleTemplate('')
    await load()
  }

  function togglePinned(id: string) {
    setPinned((cur) => (cur.includes(id) ? cur.filter((d) => d !== id) : [...cur, id]))
  }

  async function trigger(id: string) {
    if (!client) return
    const r = await client.triggerSchedule(id)
    setNote(r.produced ? 'Ran — summary produced.' : 'Ran — no messages in the window, nothing produced.')
    setTimeout(() => setNote(null), 2500)
  }

  async function showRuns(id: string) {
    if (!client) return
    const r = await client.listRuns(id)
    setRuns((cur) => ({ ...cur, [id]: r }))
  }

  return (
    <div className="mx-auto max-w-3xl space-y-6">
      <form onSubmit={create} className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <p className="text-sm font-medium text-slate-700">New schedule</p>
        <div className="mt-2 grid grid-cols-2 gap-2 sm:grid-cols-4">
          <select
            value={type}
            onChange={(e) => setType(e.target.value)}
            className="rounded-lg border border-slate-300 px-2 py-2 text-sm"
          >
            <option value="hourly">Hourly</option>
            <option value="daily">Daily</option>
          </select>
          <input
            type="number"
            min={0}
            max={23}
            value={hour}
            onChange={(e) => setHour(Number(e.target.value))}
            disabled={type === 'hourly'}
            className="rounded-lg border border-slate-300 px-2 py-2 text-sm disabled:bg-slate-100"
            placeholder="hour"
          />
          <input
            type="number"
            min={0}
            max={59}
            value={minute}
            onChange={(e) => setMinute(Number(e.target.value))}
            disabled={type === 'hourly'}
            className="rounded-lg border border-slate-300 px-2 py-2 text-sm disabled:bg-slate-100"
            placeholder="min"
          />
          <input
            value={tz}
            onChange={(e) => setTz(e.target.value)}
            className="rounded-lg border border-slate-300 px-2 py-2 text-sm"
            placeholder="UTC"
          />
        </div>
        <div className="mt-2 flex items-center gap-2">
          <input
            value={allChannels ? '' : channel}
            onChange={(e) => setChannel(e.target.value)}
            disabled={allChannels}
            className="flex-1 rounded-lg border border-slate-300 px-3 py-2 text-sm disabled:bg-slate-100"
            placeholder={allChannels ? 'all channels in this workspace' : 'channel id (optional)'}
          />
          <label className="flex shrink-0 items-center gap-1 text-xs text-slate-600">
            <input
              type="checkbox"
              checked={allChannels}
              onChange={(e) => setAllChannels(e.target.checked)}
            />
            all channels
          </label>
          <button className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-fg">
            Create
          </button>
        </div>
        {/* Optional live source: fetch fresh messages before each run (ADR-128). */}
        <div className="mt-2 flex gap-2">
          <select
            value={platform}
            onChange={(e) => setPlatform(e.target.value)}
            className="rounded-lg border border-slate-300 px-2 py-2 text-sm"
            title="Pull fresh messages from a live source before each run"
          >
            <option value="">stored messages only</option>
            <option value="discord">fetch from Discord</option>
            <option value="slack">fetch from Slack</option>
          </select>
          {platform === 'discord' && (
            <input
              value={sourceId}
              onChange={(e) => setSourceId(e.target.value)}
              className="flex-1 rounded-lg border border-slate-300 px-3 py-2 text-sm"
              placeholder="guild (server) id"
            />
          )}
        </div>
        <p className="mt-1 text-xs text-slate-400">
          A live source fetches new messages before each run (needs a bot token on the source's tab).
        </p>
        {/* Optional rolling-period digest: accumulate runs into one final (ADR-101). */}
        <div className="mt-2 flex items-center gap-2">
          <select
            value={rollingPeriod}
            onChange={(e) => setRollingPeriod(e.target.value)}
            className="rounded-lg border border-slate-300 px-2 py-2 text-sm"
            title="Accumulate each run into a single rolling digest, finalized at period end"
          >
            <option value="">one summary per run</option>
            <option value="weekly">rolling: weekly digest</option>
            <option value="biweekly">rolling: biweekly digest</option>
            <option value="monthly">rolling: monthly digest</option>
          </select>
          <span className="text-xs text-slate-400">
            {rollingPeriod
              ? 'runs accumulate; the digest publishes when the period ends'
              : ''}
          </span>
        </div>
        {/* Steering options (ADR-133 §B): perspective / template / title / continuity. */}
        <div className="mt-2 grid grid-cols-2 gap-2 sm:grid-cols-4">
          <select
            value={templateId}
            onChange={(e) => setTemplateId(e.target.value)}
            className="rounded-lg border border-slate-300 px-2 py-2 text-sm"
            title="Use a saved prompt template (takes precedence over perspective)"
          >
            <option value="">no template</option>
            {templates.map((t) => (
              <option key={t.id} value={t.id}>
                template: {t.name}
              </option>
            ))}
          </select>
          <select
            value={perspective}
            onChange={(e) => setPerspective(e.target.value)}
            disabled={!!templateId}
            className="rounded-lg border border-slate-300 px-2 py-2 text-sm disabled:opacity-50"
            title="Built-in perspective (audience/voice)"
          >
            <option value="">default perspective</option>
            {perspectives.map((p) => (
              <option key={p.id} value={p.id}>
                {p.label}
              </option>
            ))}
          </select>
          <input
            value={titleTemplate}
            onChange={(e) => setTitleTemplate(e.target.value)}
            placeholder="Title (optional)"
            className="rounded-lg border border-slate-300 px-2 py-2 text-sm"
            title="A title applied to each produced summary"
          />
          <label className="flex items-center gap-2 text-sm text-slate-600" title="Carry the previous digest forward as context">
            <input type="checkbox" checked={continuity} onChange={(e) => setContinuity(e.target.checked)} />
            Continuity
          </label>
        </div>
        {/* Delivery scope (ADR-014): pin to chosen destinations, or all by default. */}
        {dests.length > 0 && (
          <div className="mt-2">
            <p className="text-xs text-slate-500">
              Deliver to{' '}
              {pinned.length === 0 ? (
                <span className="text-slate-400">all enabled destinations</span>
              ) : (
                <span className="text-slate-600">{pinned.length} selected</span>
              )}
            </p>
            <div className="mt-1 flex flex-wrap gap-1">
              {dests.map((d) => {
                const on = pinned.includes(d.id)
                return (
                  <button
                    key={d.id}
                    type="button"
                    onClick={() => togglePinned(d.id)}
                    className={`rounded-full px-2.5 py-1 text-xs ring-1 ${
                      on
                        ? 'bg-accent text-accent-fg ring-accent'
                        : 'bg-white text-slate-600 ring-slate-300'
                    }`}
                    title={d.hint ?? d.kind}
                  >
                    {on ? '✓ ' : ''}
                    {d.kind}
                    {d.hint ? ` · ${d.hint}` : ''}
                  </button>
                )
              })}
            </div>
          </div>
        )}
      </form>

      {note && (
        <p className="rounded-lg bg-accent/10 px-3 py-2 text-sm text-slate-700">{note}</p>
      )}

      {items.length === 0 ? (
        <p className="py-12 text-center text-sm text-slate-400">No schedules yet.</p>
      ) : (
        <ul className="space-y-3">
          {items.map((s) => (
            <li key={s.id} className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <p className="font-medium text-slate-800">
                    {s.schedule_type} {s.schedule_type !== 'hourly' && `${pad(s.hour)}:${pad(s.minute)}`}{' '}
                    <span className="text-slate-400">{s.timezone}</span>
                  </p>
                  <p className="mt-0.5 text-xs text-slate-400">
                    {s.enabled ? 'enabled' : 'paused'} · next {when(s.next_run)}
                    {s.channel === '*'
                      ? ' · all channels'
                      : s.channel
                        ? ` · #${s.channel}`
                        : ' · (unscoped)'}
                    {s.platform && ` · ↻ ${s.platform}`}
                    {s.rolling_period && ` · 📅 rolling ${s.rolling_period}`}
                    {s.prompt_template_id && ' · ✎ template'}
                    {s.perspective && ` · 🎭 ${s.perspective}`}
                    {s.enable_continuity && ' · 🔗 continuity'}
                    {s.title_template && ` · “${s.title_template}”`}
                    {s.destinations.length > 0
                      ? ` · → ${s.destinations.length} destination${s.destinations.length > 1 ? 's' : ''}`
                      : ' · → all destinations'}
                  </p>
                </div>
                <div className="flex shrink-0 flex-wrap justify-end gap-1">
                  <button
                    onClick={() => void trigger(s.id)}
                    className="rounded bg-accent px-2 py-1 text-xs font-medium text-accent-fg"
                  >
                    Run now
                  </button>
                  <button
                    onClick={() => void client!.setScheduleEnabled(s.id, !s.enabled).then(load)}
                    className="rounded border border-slate-300 px-2 py-1 text-xs"
                  >
                    {s.enabled ? 'Pause' : 'Resume'}
                  </button>
                  <button
                    onClick={() => void showRuns(s.id)}
                    className="rounded border border-slate-300 px-2 py-1 text-xs"
                  >
                    History
                  </button>
                  <button
                    onClick={() => void client!.deleteSchedule(s.id).then(load)}
                    className="rounded px-2 py-1 text-xs hover:bg-red-50"
                  >
                    🗑
                  </button>
                </div>
              </div>

              {runs[s.id] && (
                <div className="mt-3 border-t border-slate-100 pt-2 text-xs">
                  {runs[s.id].length === 0 ? (
                    <p className="text-slate-400">No runs recorded.</p>
                  ) : (
                    <ul className="space-y-1">
                      {runs[s.id].map((r) => (
                        <li key={r.id} className="flex justify-between text-slate-500">
                          <span>
                            {r.status}
                            {r.manual && ' (manual)'}
                          </span>
                          <span>{when(r.ran_at)}</span>
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

function pad(n: number): string {
  return n.toString().padStart(2, '0')
}
