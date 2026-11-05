'use client'
// The channel's files as one shared screen: the tree, the open file, and who
// controls it. Everything here goes through the core's view operations, the
// same ones agents use, so what you see is what they see.
import { useEffect, useMemo, useState } from 'react'
import { core, errorText, onChanged, onView, type ChannelView, type Nav } from '@/lib/core'
import { Markdown } from '@/lib/markdown'

type Node = { name: string; path: string; folder: boolean; children: Node[] }

/** "docs/a.md", "docs/sub/b.md" → a tree, folders first. */
function tree(paths: string[]): Node[] {
  const root: Node = { name: '', path: '', folder: true, children: [] }
  for (const p of paths) {
    const parts = p.split('/')
    let at = root
    parts.forEach((name, i) => {
      const path = parts.slice(0, i + 1).join('/')
      const folder = i < parts.length - 1
      let next = at.children.find(c => c.name === name && c.folder === folder)
      if (!next) { next = { name, path, folder, children: [] }; at.children.push(next) }
      at = next
    })
  }
  const sort = (n: Node) => { n.children.sort((a, b) => Number(b.folder) - Number(a.folder) || a.name.localeCompare(b.name)); n.children.forEach(sort) }
  sort(root)
  return root.children
}

export function FilesPane({ channelId, onEditing }: { channelId: string; onEditing?: () => void }) {
  const [view, setView] = useState<ChannelView | null>(null)
  const [draft, setDraft] = useState<string | null>(null)
  const [error, setError] = useState('')

  useEffect(() => {
    const load = () => core.channelView(channelId).then(setView, e => setError(errorText(e)))
    load()
    const a = onChanged(id => { if (id === channelId) load() })
    const b = onView(id => { if (id === channelId) load() })
    return () => { a(); b() }
  }, [channelId])

  const nodes = useMemo(() => tree(view?.files.map(f => f.path) ?? []), [view?.files])
  const agent = view?.state.controller.kind === 'agent' ? view.state.controller.name : null

  async function go(nav: Nav) {
    setError('')
    try { setView(await core.driveView(channelId, nav)) } catch (e) { setError(errorText(e)) }
  }
  async function save() {
    if (!view?.state.open || draft === null) return
    setError('')
    try { setView(await core.editFile(channelId, view.state.open, 'set_text', { text: draft })); setDraft(null) } catch (e) { setError(errorText(e)) }
  }

  const row = (n: Node, depth: number): React.ReactNode => {
    const open = view?.state.expanded.includes(n.path)
    const on = view?.state.selected === n.path
    return (
      <li key={n.path}>
        <button
          className={on ? 'frow on' : 'frow'}
          style={{ paddingLeft: 8 + depth * 14 }}
          aria-expanded={n.folder ? !!open : undefined}
          onClick={() => go(n.folder ? (open ? { op: 'collapse', path: n.path } : { op: 'expand', path: n.path }) : { op: 'open', path: n.path })}
        >
          <span className="ficon" aria-hidden="true">{n.folder ? (open ? '▾' : '▸') : '·'}</span>
          {n.name}
        </button>
        {n.folder && open && <ul>{n.children.map(c => row(c, depth + 1))}</ul>}
      </li>
    )
  }

  if (!view) return error ? <p className="error" style={{ padding: 16 }}>{error}</p> : null
  const path = view.state.open
  return (
    <div className={agent ? 'files controlled' : 'files'}>
      <nav className="ftree" aria-label="Files">
        {view.files.length === 0
          ? <p className="faint" style={{ padding: 12, fontSize: 13 }}>No files yet. Docs you write appear here, and agents can open and edit them.</p>
          : <ul>{nodes.map(n => row(n, 0))}</ul>}
      </nav>
      <section className="fopen" aria-label={path ? `Open file: ${path}` : 'No file open'}>
        {!path && <p className="faint" style={{ padding: 16 }}>Open a file from the list.</p>}
        {path && (
          <>
            <header className="fhead">
              <span className="mono fpath">{path}</span>
              <span className="spacer" />
              {view.operations.includes('set_text') && draft === null && <button className="ghost" onClick={() => { setDraft(view.text ?? ''); onEditing?.() }}>Edit</button>}
              {draft !== null && <><button className="ghost" onClick={() => setDraft(null)}>Cancel</button><button className="primary" onClick={save}>Save</button></>}
              <button className="ghost" onClick={() => go({ op: 'close' })} aria-label="Close the file">Close</button>
            </header>
            <div className="fbody">
              {draft !== null
                ? <textarea className="field mono" aria-label={`Edit ${path}`} value={draft} onChange={e => setDraft(e.target.value)} />
                : view.text === null
                  ? <p className="faint">gild can’t show this file yet.</p>
                  : view.file_type === 'markdown'
                    ? <Markdown text={view.text} />
                    : <pre className="mono">{view.text}</pre>}
            </div>
          </>
        )}
      </section>
      {error && <p className="error ferror">{error}</p>}
      {agent && (
        <div className="controlbar" role="status">
          <span className="dots" aria-hidden="true"><i /><i /><i /></span>
          <span>Controlled by <b>@{agent}</b></span>
          <button className="ghost" onClick={async () => { setError(''); try { setView(await core.takeOverView(channelId)) } catch (e) { setError(errorText(e)) } }}>Take over</button>
        </div>
      )}
    </div>
  )
}
