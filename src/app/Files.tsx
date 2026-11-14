'use client'
// The Files add-on's pane: the channel's files as one shared screen (the
// tree, the open file, who controls it) with what a file manager does:
// new file and folder, upload or drop files in, rename, move, delete, and
// each file's history from git. Navigation goes through the core's view
// operations, the same ones agents use, so what you see is what they see;
// every change is one of the Files add-on's operations, a commit by you.
import { useEffect, useMemo, useRef, useState } from 'react'
import { core, clock, day, errorText, fileBase64, fileSize, onChanged, onView, KEEP, type Change, type ChannelView, type FilePreview, type Nav } from '@/lib/core'
import { Markdown } from '@/lib/markdown'

type Entry = { name: string; path: string; folder: boolean; children: Entry[] }
/** Rows a folder shows before "Show more". */
const PAGE = 200
/** The most a file in a channel can be (the core's MAX_OUTSIDE_FILE). */
const MAX_UPLOAD = 50_000_000

/** "docs/a.md", "docs/sub/b.md" → a tree, folders first. A `.gitkeep` is
 *  the folder it keeps, not a file to show. */
function tree(paths: string[]): Entry[] {
  const root: Entry = { name: '', path: '', folder: true, children: [] }
  for (const p of paths) {
    const parts = p.split('/')
    let at = root
    parts.forEach((name, i) => {
      const path = parts.slice(0, i + 1).join('/')
      const folder = i < parts.length - 1
      if (!folder && name === KEEP) return
      let next = at.children.find(c => c.name === name && c.folder === folder)
      if (!next) { next = { name, path, folder, children: [] }; at.children.push(next) }
      at = next
    })
  }
  const sort = (n: Entry) => { n.children.sort((a, b) => Number(b.folder) - Number(a.folder) || a.name.localeCompare(b.name)); n.children.forEach(sort) }
  sort(root)
  return root.children
}

const parentOf = (path: string) => path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : ''
const nameOf = (path: string) => path.slice(path.lastIndexOf('/') + 1)
const join = (folder: string, name: string) => (folder ? `${folder}/${name}` : name)

type Action = 'new-file' | 'new-folder' | 'rename' | 'move' | 'delete' | null

export function FilesPane({ channelId, onEditing }: { channelId: string; onEditing?: () => void }) {
  const [view, setView] = useState<ChannelView | null>(null)
  const [preview, setPreview] = useState<FilePreview | null>(null)
  const [draft, setDraft] = useState<string | null>(null)
  const [history, setHistory] = useState<Change[] | null>(null)
  const [action, setAction] = useState<Action>(null)
  const [name, setName] = useState('')
  const [busy, setBusy] = useState(false)
  const [dragging, setDragging] = useState(false)
  const [shown, setShown] = useState<Record<string, number>>({})
  const [error, setError] = useState('')
  const picker = useRef<HTMLInputElement>(null)
  const nameBox = useRef<HTMLInputElement>(null)

  useEffect(() => {
    const load = () => core.channelView(channelId).then(setView, e => setError(errorText(e)))
    load()
    const a = onChanged(id => { if (id === channelId) load() })
    const b = onView(id => { if (id === channelId) load() })
    return () => { a(); b() }
  }, [channelId])

  // The open file, as the add-on for its type shows it.
  const open = view?.state.open ?? null
  useEffect(() => {
    setHistory(null)
    if (!open) { setPreview(null); return }
    let gone = false
    core.filePreview(channelId, open).then(p => { if (!gone) setPreview(p) }, e => { if (!gone) { setPreview(null); setError(errorText(e)) } })
    return () => { gone = true }
  }, [channelId, open, view?.text])
  useEffect(() => { if (action && action !== 'delete') nameBox.current?.focus() }, [action])

  const nodes = useMemo(() => tree(view?.files.map(f => f.path) ?? []), [view?.files])
  const folders = useMemo(() => new Set((view?.files ?? []).flatMap(f => { const parts = f.path.split('/'); return parts.slice(0, -1).map((_, i) => parts.slice(0, i + 1).join('/')) })), [view?.files])
  const agent = view?.state.controller.kind === 'agent' ? view.state.controller.name : null
  const selected = view?.state.selected ?? null
  const selectedIsFolder = selected !== null && folders.has(selected)
  // Where new things go: the selected folder, or the open file's.
  const into = selected === null ? '' : selectedIsFolder ? selected : parentOf(selected)

  async function go(nav: Nav) {
    setError('')
    try { setView(await core.driveView(channelId, nav)) } catch (e) { setError(errorText(e)) }
  }
  async function save() {
    if (!view?.state.open || draft === null) return
    setError('')
    try { setView(await core.editFile(channelId, view.state.open, 'set_text', { text: draft })); setDraft(null) } catch (e) { setError(errorText(e)) }
  }
  async function run(f: () => Promise<unknown>) {
    setBusy(true)
    setError('')
    try { await f(); setAction(null); setName('') } catch (e) { setError(errorText(e)) } finally { setBusy(false) }
  }
  function start(a: Action) {
    setError('')
    setAction(a)
    setName(a === 'rename' && selected ? nameOf(selected) : a === 'move' && selected ? selected : '')
  }
  async function submit(e: React.FormEvent) {
    e.preventDefault()
    const typed = name.trim()
    if (!typed) return
    if (action === 'new-file') {
      const file = typed.includes('.') ? typed : `${typed}.md`
      const path = join(into, file)
      const title = file.replace(/\.md$/i, '')
      await run(async () => {
        await core.writeFile(channelId, path, file.toLowerCase().endsWith('.md') ? `# ${title}\n\n` : '')
        setView(await core.driveView(channelId, { op: 'open', path }))
        onEditing?.()
      })
    } else if (action === 'new-folder') {
      const path = join(into, typed)
      await run(async () => { await core.newFolder(channelId, path); setView(await core.driveView(channelId, { op: 'select', path })) })
    } else if (action === 'rename' && selected) {
      const to = join(parentOf(selected), typed)
      await run(async () => { await core.moveFile(channelId, selected, to); setView(await core.driveView(channelId, { op: 'select', path: to })) })
    } else if (action === 'move' && selected) {
      await run(async () => { await core.moveFile(channelId, selected, typed); setView(await core.driveView(channelId, { op: 'select', path: typed })) })
    }
  }
  async function remove() {
    if (!selected) return
    await run(() => core.deleteFile(channelId, selected))
  }
  async function upload(list: FileList | File[]) {
    const files = Array.from(list)
    if (files.length === 0) return
    const big = files.find(f => f.size > MAX_UPLOAD)
    if (big) { setError(`${big.name} is bigger than ${MAX_UPLOAD / 1_000_000} MB, the most a file in a channel can be.`); return }
    await run(async () => {
      const data = await Promise.all(files.map(async f => ({ name: f.name, data: await fileBase64(f) })))
      const added = await core.uploadFiles(channelId, into, data)
      if (added.length === 1) setView(await core.driveView(channelId, { op: 'open', path: added[0].path }))
    })
  }
  async function toggleHistory() {
    if (history) return setHistory(null)
    if (!open) return
    setError('')
    try { setHistory(await core.fileHistory(channelId, open)) } catch (e) { setError(errorText(e)) }
  }
  async function show() {
    if (!open) return
    setError('')
    try { await core.showFile(channelId, open) } catch (e) { setError(errorText(e)) }
  }

  const row = (n: Entry, depth: number): React.ReactNode => {
    const expanded = view?.state.expanded.includes(n.path)
    const on = selected === n.path
    const limit = shown[n.path] ?? PAGE
    const children = n.children.slice(0, limit)
    const more = n.children.length - children.length
    return (
      <li key={n.path}>
        <button
          className={on ? 'frow on' : 'frow'}
          style={{ paddingLeft: 8 + depth * 14 }}
          aria-expanded={n.folder ? !!expanded : undefined}
          aria-current={on ? 'true' : undefined}
          onClick={() => go(n.folder ? (expanded ? { op: 'collapse', path: n.path } : { op: 'expand', path: n.path }) : { op: 'open', path: n.path })}
        >
          <span className="ficon" aria-hidden="true">{n.folder ? (expanded ? '▾' : '▸') : '·'}</span>
          <span className="fname">{n.name}</span>
        </button>
        {n.folder && expanded && (
          <ul>
            {children.map(c => row(c, depth + 1))}
            {more > 0 && <li><button className="frow fmore" style={{ paddingLeft: 8 + (depth + 1) * 14 }} onClick={() => setShown(s => ({ ...s, [n.path]: limit + PAGE }))}>Show {Math.min(more, PAGE)} more of {n.children.length}</button></li>}
          </ul>
        )}
      </li>
    )
  }

  if (!view) return error ? <p className="error" style={{ padding: 16 }}>{error}</p> : null
  const path = view.state.open
  const top = nodes.slice(0, shown[''] ?? PAGE)
  const moreTop = nodes.length - top.length
  const label: Record<Exclude<Action, null | 'delete'>, string> = { 'new-file': `New file in ${into || 'the top folder'}`, 'new-folder': `New folder in ${into || 'the top folder'}`, rename: `Rename ${selected ? nameOf(selected) : ''}`, move: `Move ${selected ?? ''} to` }
  return (
    <div
      className={['files', agent ? 'controlled' : '', dragging ? 'dragging' : ''].filter(Boolean).join(' ')}
      onDragOver={e => { if (e.dataTransfer.types.includes('Files')) { e.preventDefault(); setDragging(true) } }}
      onDragLeave={e => { if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDragging(false) }}
      onDrop={e => { e.preventDefault(); setDragging(false); upload(e.dataTransfer.files) }}
    >
      <nav className="ftree" aria-label="Files">
        <div className="fbar" role="toolbar" aria-label="File actions">
          <button className="ghost" onClick={() => start('new-file')} disabled={busy}>New file</button>
          <button className="ghost" onClick={() => start('new-folder')} disabled={busy}>New folder</button>
          <button className="ghost" onClick={() => picker.current?.click()} disabled={busy}>Upload</button>
          <input ref={picker} type="file" multiple hidden aria-label="Files to upload" onChange={e => { if (e.target.files) upload(e.target.files); e.target.value = '' }} />
          {selected && (
            <>
              <button className="ghost" onClick={() => start('rename')} disabled={busy}>Rename</button>
              <button className="ghost" onClick={() => start('move')} disabled={busy}>Move</button>
              <button className="ghost" onClick={() => start('delete')} disabled={busy}>Delete</button>
            </>
          )}
        </div>
        {action && action !== 'delete' && (
          <form className="faction" onSubmit={submit}>
            <label htmlFor="file-name">{label[action]}</label>
            <input id="file-name" ref={nameBox} className="field mono" value={name} onChange={e => setName(e.target.value)} spellCheck={false} placeholder={action === 'new-file' ? 'name.md' : action === 'new-folder' ? 'name' : undefined} />
            <div className="row">
              <button className="primary" disabled={busy || !name.trim()}>{action === 'new-file' ? 'Create' : action === 'new-folder' ? 'Create' : action === 'rename' ? 'Rename' : 'Move'}</button>
              <button type="button" className="ghost" onClick={() => setAction(null)}>Cancel</button>
            </div>
          </form>
        )}
        {action === 'delete' && selected && (
          <div className="faction fconfirm" role="alertdialog" aria-label={`Delete ${selected}?`}>
            <span>Delete <span className="mono">{selected}</span>{selectedIsFolder ? ' and everything in it' : ''}? It stays in the channel’s history.</span>
            <div className="row">
              <button className="danger" onClick={remove} disabled={busy}>Delete {nameOf(selected)}</button>
              <button className="ghost" onClick={() => setAction(null)}>Cancel</button>
            </div>
          </div>
        )}
        {view.files.length === 0
          ? <p className="faint" style={{ padding: 12, fontSize: 13 }}>No files yet. Create one, or drop files here; docs you write appear here too, and agents can open and edit them.</p>
          : <ul>{top.map(n => row(n, 0))}{moreTop > 0 && <li><button className="frow fmore" onClick={() => setShown(s => ({ ...s, '': (s[''] ?? PAGE) + PAGE }))}>Show {Math.min(moreTop, PAGE)} more of {nodes.length}</button></li>}</ul>}
        {dragging && <p className="fdrop" aria-live="polite">Drop to add to {into || 'the top folder'}</p>}
      </nav>
      <section className="fopen" aria-label={path ? `Open file: ${path}` : 'No file open'}>
        {!path && <p className="faint" style={{ padding: 16 }}>Open a file from the list.</p>}
        {path && (
          <>
            <header className="fhead">
              <span className="mono fpath">{path}</span>
              {preview && <span className="faint" style={{ fontSize: 12 }}>{fileSize(preview.size)}</span>}
              <span className="spacer" />
              {view.operations.includes('set_text') && draft === null && <button className="ghost" onClick={() => { setDraft(view.text ?? ''); onEditing?.() }}>Edit</button>}
              {draft !== null && <><button className="ghost" onClick={() => setDraft(null)}>Cancel</button><button className="primary" onClick={save}>Save</button></>}
              {draft === null && <button className="ghost" onClick={toggleHistory}>{history ? 'Hide history' : 'History'}</button>}
              <button className="ghost" onClick={() => go({ op: 'close' })} aria-label="Close the file">Close</button>
            </header>
            {history && (
              <div className="history inline fhistory" aria-label={`History of ${path}`}>
                {history.length === 0 && <span className="faint">No changes yet.</span>}
                {history.map(h => (
                  <div key={h.commit} className="change">
                    <span>{h.message}</span>
                    <span className="faint mono" style={{ fontSize: 11 }}>{h.author} · {day(h.at)} {clock(h.at)} · {h.commit.slice(0, 7)}</span>
                  </div>
                ))}
              </div>
            )}
            <div className="fbody">
              {draft !== null
                ? <textarea className="field mono" aria-label={`Edit ${path}`} value={draft} onChange={e => setDraft(e.target.value)} />
                : view.file_type === 'markdown' && view.text !== null
                  ? <Markdown text={view.text} />
                  : !preview
                    ? null
                    : preview.kind === 'text'
                      ? <pre className="mono">{preview.text}</pre>
                      : preview.kind === 'image'
                        ? <img className="fimg" src={preview.data ?? ''} alt={nameOf(path)} />
                        : (
                          <div className="fother">
                            <b>{nameOf(path)}</b>
                            <span className="soft">{fileSize(preview.size)} · gild doesn’t show this kind of file.</span>
                            <button className="ghost" onClick={show}>{preview.show}</button>
                          </div>
                        )}
            </div>
          </>
        )}
      </section>
      {error && <p className="error ferror" role="alert">{error}</p>}
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
