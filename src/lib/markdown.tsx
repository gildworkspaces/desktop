'use client'
// Markdown for docs, rendered as React elements: never as HTML strings, so
// nothing in a doc (written by a person or proposed by an agent) runs as
// code. It covers what SOPs and notes use: headings, paragraphs, lists,
// quotes, code, rules, bold, italic and links.
import type { ReactNode } from 'react'
import { openUrl } from '@tauri-apps/plugin-opener'

const INLINE = /(`[^`]+`)|(\*\*[^*]+\*\*)|(\*[^*\s][^*]*\*|_[^_\s][^_]*_)|(\[[^\]]+\]\([^)\s]+\))/g

/** Links open in the browser, and only web and mail links open at all. */
function Link({ href, children }: { href: string; children: ReactNode }) {
  const ok = /^(https?:|mailto:)/i.test(href)
  if (!ok) return <span title={href}>{children}</span>
  return <a href={href} onClick={e => { e.preventDefault(); openUrl(href) }}>{children}</a>
}

export function inline(text: string): ReactNode[] {
  const out: ReactNode[] = []
  let last = 0
  for (const m of text.matchAll(INLINE)) {
    const at = m.index ?? 0
    if (at > last) out.push(text.slice(last, at))
    const s = m[0]
    const k = out.length
    if (m[1]) out.push(<code key={k}>{s.slice(1, -1)}</code>)
    else if (m[2]) out.push(<strong key={k}>{inline(s.slice(2, -2))}</strong>)
    else if (m[3]) out.push(<em key={k}>{inline(s.slice(1, -1))}</em>)
    else {
      const close = s.indexOf('](')
      out.push(<Link key={k} href={s.slice(close + 2, -1)}>{inline(s.slice(1, close))}</Link>)
    }
    last = at + s.length
  }
  if (last < text.length) out.push(text.slice(last))
  return out
}

export function Markdown({ text }: { text: string }) {
  const lines = text.replace(/\r\n?/g, '\n').split('\n')
  const blocks: ReactNode[] = []
  let i = 0
  const key = () => blocks.length
  while (i < lines.length) {
    const line = lines[i]
    const t = line.trim()
    if (!t) { i++; continue }
    if (t.startsWith('```')) {
      const code: string[] = []
      i++
      while (i < lines.length && !lines[i].trim().startsWith('```')) code.push(lines[i++])
      i++
      blocks.push(<pre key={key()}><code>{code.join('\n')}</code></pre>)
      continue
    }
    const h = /^(#{1,6})\s+(.*)$/.exec(t)
    if (h) {
      const level = Math.min(h[1].length + 1, 6)
      const Tag = `h${level}` as 'h2'
      blocks.push(<Tag key={key()}>{inline(h[2])}</Tag>)
      i++
      continue
    }
    if (/^(-{3,}|\*{3,}|_{3,})$/.test(t)) { blocks.push(<hr key={key()} />); i++; continue }
    if (t.startsWith('>')) {
      const quote: string[] = []
      while (i < lines.length && lines[i].trim().startsWith('>')) quote.push(lines[i++].trim().replace(/^>\s?/, ''))
      blocks.push(<blockquote key={key()}>{inline(quote.join(' '))}</blockquote>)
      continue
    }
    const bullet = /^[-*+]\s+/
    const numbered = /^\d+[.)]\s+/
    if (bullet.test(t) || numbered.test(t)) {
      const ordered = numbered.test(t)
      const re = ordered ? numbered : bullet
      const items: string[] = []
      while (i < lines.length && re.test(lines[i].trim())) items.push(lines[i++].trim().replace(re, ''))
      const List = ordered ? 'ol' : 'ul'
      blocks.push(<List key={key()}>{items.map((x, j) => <li key={j}>{inline(x)}</li>)}</List>)
      continue
    }
    const para: string[] = []
    while (i < lines.length && lines[i].trim() && !/^(#{1,6}\s|```|>|[-*+]\s|\d+[.)]\s)/.test(lines[i].trim())) para.push(lines[i++].trim())
    if (para.length === 0) para.push(lines[i++].trim())
    blocks.push(<p key={key()}>{inline(para.join(' '))}</p>)
  }
  return <div className="md">{blocks}</div>
}
