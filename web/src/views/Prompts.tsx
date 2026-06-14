import { useCallback, useEffect, useState } from 'react'
import { useAuth } from '../auth'
import type { Prompts as PromptsData, PromptTemplate } from '../types'

// Prompts view (ADR-133 A2): named, reusable instruction templates beyond the
// built-in perspectives (General/Developer/Marketing/Executive/Support). Steer a
// summary's audience/voice; selectable on the Create/compose flow.

export function Prompts() {
  const { client } = useAuth()
  const [data, setData] = useState<PromptsData | null>(null)
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)
  // Editor state: null = closed, '' id = new, else editing existing.
  const [editing, setEditing] = useState<{ id: string | null; name: string; content: string } | null>(
    null,
  )

  const load = useCallback(async () => {
    if (!client) return
    setBusy(true)
    setErr(null)
    try {
      setData(await client.listPrompts())
    } catch {
      setErr('Could not load prompts.')
    } finally {
      setBusy(false)
    }
  }, [client])

  useEffect(() => {
    void load()
  }, [load])

  async function save() {
    if (!client || !editing) return
    if (!editing.name.trim() || !editing.content.trim()) {
      setErr('Name and content are required.')
      return
    }
    setBusy(true)
    setErr(null)
    try {
      if (editing.id) {
        await client.updatePrompt(editing.id, { name: editing.name, content: editing.content })
      } else {
        await client.createPrompt({ name: editing.name, content: editing.content })
      }
      setEditing(null)
      await load()
    } catch {
      setErr('Save failed.')
    } finally {
      setBusy(false)
    }
  }

  async function remove(t: PromptTemplate) {
    if (!client) return
    await client.deletePrompt(t.id)
    await load()
  }

  return (
    <div className="mx-auto max-w-3xl space-y-4">
      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <div className="flex items-start justify-between gap-3">
          <div>
            <h2 className="font-semibold text-slate-800">Prompts &amp; Perspectives</h2>
            <p className="mt-1 text-sm text-slate-500">
              Reusable instruction templates that steer a summary's audience and voice — beyond the
              built-in perspectives.
            </p>
          </div>
          <button
            onClick={() => setEditing({ id: null, name: '', content: '' })}
            className="shrink-0 rounded-lg bg-accent px-3 py-1.5 text-sm font-medium text-accent-fg"
          >
            New template
          </button>
        </div>
        {err && <p className="mt-3 text-sm text-red-600">{err}</p>}

        {data && (
          <div className="mt-3">
            <div className="text-xs font-medium uppercase tracking-wide text-slate-400">
              Built-in perspectives
            </div>
            <div className="mt-1 flex flex-wrap gap-1.5">
              {data.perspectives.map((p) => (
                <span
                  key={p.id}
                  className="rounded-full bg-slate-100 px-2.5 py-1 text-xs font-medium text-slate-600"
                >
                  {p.label}
                </span>
              ))}
            </div>
          </div>
        )}
      </div>

      {editing && (
        <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-accent/40">
          <h3 className="text-sm font-medium text-slate-700">
            {editing.id ? 'Edit template' : 'New template'}
          </h3>
          <input
            value={editing.name}
            onChange={(e) => setEditing({ ...editing, name: e.target.value })}
            placeholder="Name (e.g. Security focus)"
            className="mt-2 w-full rounded-lg border border-slate-300 px-3 py-2 text-sm outline-none focus:border-accent"
          />
          <textarea
            value={editing.content}
            onChange={(e) => setEditing({ ...editing, content: e.target.value })}
            rows={4}
            placeholder="Instructions — e.g. Focus on security issues, CVEs, and mitigations."
            className="mt-2 w-full resize-y rounded-lg border border-slate-300 p-2 text-sm outline-none focus:border-accent"
          />
          <div className="mt-2 flex gap-2">
            <button
              onClick={save}
              disabled={busy}
              className="rounded-lg bg-accent px-3 py-1.5 text-sm font-medium text-accent-fg disabled:opacity-50"
            >
              {busy ? 'Saving…' : 'Save'}
            </button>
            <button
              onClick={() => setEditing(null)}
              className="rounded-lg border border-slate-300 px-3 py-1.5 text-sm"
            >
              Cancel
            </button>
          </div>
        </div>
      )}

      <div className="rounded-xl bg-white p-4 shadow-sm ring-1 ring-slate-200">
        <h3 className="text-sm font-medium text-slate-600">Your templates</h3>
        {data?.templates.length === 0 && (
          <p className="mt-3 text-sm text-slate-400">
            No templates yet — create one to reuse custom summary instructions.
          </p>
        )}
        <ul className="mt-2 divide-y divide-slate-100">
          {data?.templates.map((t) => (
            <li key={t.id} className="py-3 text-sm">
              <div className="flex items-center justify-between gap-3">
                <span className="font-medium text-slate-800">{t.name}</span>
                <div className="flex shrink-0 items-center gap-2">
                  <span className="text-xs text-slate-400">used {t.usage_count}×</span>
                  <button
                    onClick={() => setEditing({ id: t.id, name: t.name, content: t.content })}
                    className="rounded border border-slate-300 px-2 py-0.5 text-xs"
                  >
                    Edit
                  </button>
                  <button
                    onClick={() => void remove(t)}
                    className="rounded border border-red-200 px-2 py-0.5 text-xs text-red-600"
                  >
                    Delete
                  </button>
                </div>
              </div>
              <p className="mt-1 whitespace-pre-wrap text-xs text-slate-500">{t.content}</p>
            </li>
          ))}
        </ul>
      </div>
    </div>
  )
}
