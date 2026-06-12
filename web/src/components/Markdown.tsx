// Minimal, dependency-free markdown renderer shared across views (the knowledge
// base, the rolling/weekly digest in Summaries, etc.): headings (#/##/###), bullet
// lists, and inline **bold** / _italic_. Enough for the LLM's topic-grouped output
// without pulling in a markdown library.
export function Markdown({ content }: { content: string }) {
  const lines = content.split('\n')
  const blocks: React.ReactNode[] = []
  let bullets: string[] = []

  const flushBullets = () => {
    if (bullets.length === 0) return
    blocks.push(
      <ul key={`ul-${blocks.length}`} className="my-2 list-disc space-y-1 pl-5 text-sm text-slate-700">
        {bullets.map((b, i) => (
          <li key={i}>{inline(b)}</li>
        ))}
      </ul>,
    )
    bullets = []
  }

  for (const raw of lines) {
    const line = raw.trimEnd()
    if (/^#{1}\s+/.test(line)) {
      flushBullets()
      blocks.push(
        <h3 key={blocks.length} className="mt-3 text-lg font-semibold text-slate-800">
          {inline(line.replace(/^#\s+/, ''))}
        </h3>,
      )
    } else if (/^#{2}\s+/.test(line)) {
      flushBullets()
      blocks.push(
        <h4 key={blocks.length} className="mt-3 font-semibold text-slate-700">
          {inline(line.replace(/^#{2}\s+/, ''))}
        </h4>,
      )
    } else if (/^#{3,}\s+/.test(line)) {
      flushBullets()
      blocks.push(
        <h5 key={blocks.length} className="mt-2 text-sm font-semibold text-slate-600">
          {inline(line.replace(/^#{3,}\s+/, ''))}
        </h5>,
      )
    } else if (/^[-*]\s+/.test(line)) {
      bullets.push(line.replace(/^[-*]\s+/, ''))
    } else if (line.trim() === '') {
      flushBullets()
    } else {
      flushBullets()
      blocks.push(
        <p key={blocks.length} className="my-2 text-sm text-slate-700">
          {inline(line)}
        </p>,
      )
    }
  }
  flushBullets()
  return <div>{blocks}</div>
}

// Inline emphasis: **bold** and _italic_ / *italic*. Splits on those markers and
// wraps the captured runs; everything else is plain text.
function inline(text: string): React.ReactNode {
  const parts: React.ReactNode[] = []
  const re = /\*\*(.+?)\*\*|_(.+?)_|\*(.+?)\*/g
  let last = 0
  let m: RegExpExecArray | null
  let i = 0
  while ((m = re.exec(text)) !== null) {
    if (m.index > last) parts.push(text.slice(last, m.index))
    if (m[1] !== undefined) {
      parts.push(
        <strong key={i++} className="font-semibold text-slate-800">
          {m[1]}
        </strong>,
      )
    } else {
      parts.push(<em key={i++}>{m[2] ?? m[3]}</em>)
    }
    last = re.lastIndex
  }
  if (last < text.length) parts.push(text.slice(last))
  return parts
}
