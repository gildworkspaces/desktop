'use client'
import { useCallback, useEffect, useRef, useState } from 'react'
import { ViewPane } from './ChannelView'
import { DocsPane } from './Docs'
import { AgentsSidebar, AccessPanel, ActivityPanel } from './Agents'
import { MessageRow, ThreadPanel } from './Thread'
import { revealItemInDir } from '@tauri-apps/plugin-opener'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { core, inApp, onChanged, mentionParts, tint, initial, clock, day, errorText, type Account, type Workspace, type Channel, type ChannelDocs, type Message } from '@/lib/core'

export default function App() {
  const [ready, setReady] = useState<'checking' | 'app' | 'browser'>('checking')
  const [account, setAccount] = useState<Account | null>(null)
  const [open, setOpen] = useState<Workspace | null>(null)
  const [error, setError] = useState('')

  useEffect(() => {
    if (!inApp()) return setReady('browser')
    setReady('app')
    core.account().then(setAccount, e => setError(errorText(e)))
  }, [])

  if (ready === 'checking') return <main className="center" aria-busy="true" />
  if (ready === 'browser') return <main className="center"><p className="soft">This page is gild's desktop interface. Open it in the gild app.</p></main>
  if (error) return <main className="center"><p className="error">{error}</p></main>
  if (!account) return <main className="center" aria-busy="true" />
  return open
    ? <WorkspaceView account={account} workspace={open} onBack={() => setOpen(null)} />
    : <Home account={account} onOpen={setOpen} />
}

function Logo({ size = 28 }: { size?: number }) {
  return <img src="/logo.svg" width={size} height={size} alt="" aria-hidden="true" />
}

function Who({ account }: { account: Account }) {
  return (
    <div className="who">
      {account.demo && <span className="chip" title="Accounts arrive later. Everything is written as the demo account.">demo account</span>}
      <div className="avatar person" aria-hidden="true">{initial(account.name)}</div>
      <span>{account.name}</span>
    </div>
  )
}

function Home({ account, onOpen }: { account: Account; onOpen: (w: Workspace) => void }) {
  const [list, setList] = useState<Workspace[] | null>(null)
  const [name, setName] = useState('')
  const [error, setError] = useState('')

  useEffect(() => { core.workspaces().then(setList, e => setError(errorText(e))) }, [])

  async function create(e: React.FormEvent) {
    e.preventDefault()
    setError('')
    try {
      const ws = await core.createWorkspace(name)
      setName('')
      onOpen(ws)
    } catch (err) { setError(errorText(err)) }
  }

  return (
    <main className="home">
      <div className="topbar">
        <div className="brand"><Logo /><b>gild</b></div>
        <Who account={account} />
      </div>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
        <h1>Workspaces</h1>
        <p className="soft">Each workspace holds channels. Each channel holds a conversation and the data it is about.</p>
      </div>
      <div className="cards">
        {list?.map(w => (
          <button key={w.id} className="card" onClick={() => onOpen(w)}>
            <div style={{ display: 'flex', gap: 12, alignItems: 'center' }}>
              <div className="tile" style={{ background: tint(w.name) }} aria-hidden="true">{initial(w.name)}</div>
              <div style={{ display: 'flex', flexDirection: 'column' }}>
                <span className="name">{w.name}</span>
                <span className="faint" style={{ fontSize: 12 }}>
                  {w.channels} {w.channels === 1 ? 'channel' : 'channels'} · {w.messages} {w.messages === 1 ? 'message' : 'messages'}
                </span>
              </div>
            </div>
          </button>
        ))}
        <form className="card new" onSubmit={create}>
          <label className="label" htmlFor="new-workspace">New workspace</label>
          <div className="newform">
            <input id="new-workspace" className="field" placeholder="Name, e.g. Front Desk" value={name} onChange={e => setName(e.target.value)} />
            <button className="primary" disabled={!name.trim()}>Create workspace</button>
          </div>
          {error && <p className="error">{error}</p>}
        </form>
      </div>
      <ImportWorkspace onImported={onOpen} />
    </main>
  )
}

function ImportWorkspace({ onImported }: { onImported: (w: Workspace) => void }) {
  const [state, setState] = useState<{ busy: boolean; error?: string }>({ busy: false })
  async function pick() {
    const folder = await openDialog({ directory: true, title: 'Pick a folder Export workspace made' })
    if (typeof folder !== 'string') return
    setState({ busy: true })
    try { onImported(await core.importWorkspace(folder)) } catch (e) { setState({ busy: false, error: errorText(e) }) }
  }
  return (
    <div className="importbox">
      <button className="ghost" onClick={pick} disabled={state.busy}>{state.busy ? 'Importing…' : 'Import a workspace'}</button>
      <span className="faint" style={{ fontSize: 13 }}>From a folder Export workspace made: its channels, chat, tables, docs and full history come back.</span>
      {state.error && <p className="error" style={{ flexBasis: '100%', margin: 0 }}>{state.error}</p>}
    </div>
  )
}

function WorkspaceView({ account, workspace, onBack }: { account: Account; workspace: Workspace; onBack: () => void }) {
  const [channels, setChannels] = useState<Channel[]>([])
  const [current, setCurrent] = useState<Channel | null>(null)
  const [newName, setNewName] = useState('')
  const [error, setError] = useState('')

  useEffect(() => {
    core.channels(workspace.id).then(cs => { setChannels(cs); setCurrent(cs[0] ?? null) }, e => setError(errorText(e)))
  }, [workspace.id])

  async function addChannel(e: React.FormEvent) {
    e.preventDefault()
    setError('')
    try {
      const c = await core.createChannel(workspace.id, newName)
      setChannels(cs => [...cs, c])
      setCurrent(c)
      setNewName('')
    } catch (err) { setError(errorText(err)) }
  }

  return (
    <div className="ws">
      <nav className="side" aria-label="Channels">
        <button className="back" onClick={onBack}>← All workspaces</button>
        <div className="wsname">
          <div className="tile" style={{ background: tint(workspace.name) }} aria-hidden="true">{initial(workspace.name)}</div>
          <span>{workspace.name}</span>
        </div>
        <div className="group">
          <span className="label">Channels</span>
          {channels.map(c => (
            <button key={c.id} className={c.id === current?.id ? 'chan on' : 'chan'} onClick={() => setCurrent(c)} aria-current={c.id === current?.id ? 'page' : undefined}>
              <span className="hash" aria-hidden="true">#</span>{c.name}
            </button>
          ))}
          <form className="addchan" onSubmit={addChannel}>
            <label htmlFor="new-channel" className="label" style={{ position: 'absolute', width: 1, height: 1, overflow: 'hidden', clip: 'rect(0 0 0 0)' }}>New channel</label>
            <input id="new-channel" className="field" placeholder="+ new channel" value={newName} onChange={e => setNewName(e.target.value)} />
          </form>
          {error && <p className="error" style={{ padding: '0 10px' }}>{error}</p>}
        </div>
        <div className="group">
          <span className="label">Members</span>
          <div className="member"><span className="dot" aria-hidden="true" />{account.name} (you)</div>
        </div>
        <AgentsSidebar channels={channels} current={current} />
        <ExportWorkspace workspace={workspace} />
        <div className="foot"><Who account={account} /></div>
      </nav>
      {current ? <ChannelView key={current.id} channel={current} /> : <main className="channel" />}
    </div>
  )
}


function ExportWorkspace({ workspace }: { workspace: Workspace }) {
  const [state, setState] = useState<{ busy: boolean; path?: string; error?: string }>({ busy: false })
  async function run() {
    setState({ busy: true })
    try { setState({ busy: false, path: await core.exportWorkspace(workspace.id) }) } catch (e) { setState({ busy: false, error: errorText(e) }) }
  }
  return (
    <div className="group exportbox">
      <button className="ghost" onClick={run} disabled={state.busy}>{state.busy ? 'Exporting…' : 'Export workspace'}</button>
      <p className="note">A folder of open files: chat as markdown, tables as CSV, full history as git, and a page that browses it all offline.</p>
      {state.path && (
        <p className="note">Saved to <span className="mono">{state.path.split('/').slice(-2).join('/')}</span>. <button className="link" onClick={() => revealItemInDir(state.path!)}>Show in Finder</button></p>
      )}
      {state.error && <p className="error" style={{ padding: '0 10px' }}>{state.error}</p>}
    </div>
  )
}

function ChannelView({ channel }: { channel: Channel }) {
  const [messages, setMessages] = useState<Message[]>([])
  const [draft, setDraft] = useState('')
  const [expanded, setExpanded] = useState(false)
  const [mode, setMode] = useState<'table' | 'board' | 'docs'>('table')
  const [chosen, setChosen] = useState(false)
  // undefined: not loaded yet; null: the channel has no table.
  const [count, setCount] = useState<number | null | undefined>(undefined)
  const [docs, setDocs] = useState<ChannelDocs | null>(null)
  const [error, setError] = useState('')
  const list = useRef<HTMLDivElement>(null)

  const [panel, setPanel] = useState<'access' | 'activity' | null>(null)
  const [threadId, setThreadId] = useState<string | null>(null)
  const openThread = (m: Message) => { setPanel(null); setThreadId(m.id) }
  const [version, setVersion] = useState(0)
  const refresh = () => setVersion(v => v + 1)
  useEffect(() => { core.messages(channel.id).then(setMessages, e => setError(errorText(e))) }, [channel.id, version])
  useEffect(() => { core.channelDocs(channel.id).then(setDocs, e => setError(errorText(e))) }, [channel.id, version])
  // A channel with docs and no table opens on its docs.
  useEffect(() => { if (!chosen && count === null && docs && (docs.docs.length > 0 || docs.proposals.length > 0)) setMode('docs') }, [chosen, count, docs])
  const show = (m: 'table' | 'board' | 'docs') => { setChosen(true); setMode(m) }
  const waiting = docs?.proposals.length ?? 0
  // An agent changed this channel: show it.
  useEffect(() => onChanged(id => { if (id === channel.id) setVersion(v => v + 1) }), [channel.id])
  const toBottom = useCallback(() => { const el = list.current; if (el) el.scrollTop = el.scrollHeight }, [])
  useEffect(toBottom, [messages, toBottom])

  async function send(e: React.FormEvent) {
    e.preventDefault()
    if (!draft.trim()) return
    setError('')
    try {
      const m = await core.send(channel.id, draft)
      setMessages(ms => [...ms, m])
      setDraft('')
    } catch (err) { setError(errorText(err)) }
  }

  return (
    <main className="channel">
      <header className="chead">
        <h2># {channel.name}</h2>
        {channel.topic && <span className="soft" style={{ fontSize: 13 }}>{channel.topic}</span>}
        <span className="spacer" />
        {typeof count === 'number' && mode !== 'docs' && <span className="faint mono" style={{ fontSize: 12 }}>{count} {count === 1 ? 'record' : 'records'}</span>}
        <div className="tabs" role="group" aria-label="Show">
          <button className={mode === 'table' ? 'tab on' : 'tab'} aria-pressed={mode === 'table'} onClick={() => show('table')}>Table</button>
          {typeof count === 'number' && <button className={mode === 'board' ? 'tab on' : 'tab'} aria-pressed={mode === 'board'} onClick={() => show('board')}>Board</button>}
          <button className={mode === 'docs' ? 'tab on' : 'tab'} aria-pressed={mode === 'docs'} onClick={() => show('docs')}>
            Docs{waiting > 0 && <span className="count" aria-label={`${waiting} waiting for approval`}>{waiting}</span>}
          </button>
        </div>
        <button className="ghost" onClick={() => { setThreadId(null); setPanel(p => (p === 'access' ? null : 'access')) }}>Access</button>
        <button className="ghost" onClick={() => { setThreadId(null); setPanel(p => (p === 'activity' ? null : 'activity')) }}>Activity</button>
        <button className="ghost" onClick={() => setExpanded(x => !x)}>{expanded ? 'Collapse view' : 'Expand view'}</button>
      </header>
      <section className="viewpane" aria-label="Channel view" style={{ flex: `${expanded ? 2 : 1} 1 0` }}>
        {mode === 'docs'
          ? docs && <DocsPane channelId={channel.id} data={docs} onChanged={refresh} onEditing={() => setExpanded(true)} />
          : <ViewPane key={version} channelId={channel.id} mode={mode} onCount={setCount} />}
      </section>
      <section className="msgs" aria-label="Messages" style={{ flex: `${expanded ? 1 : 2} 1 0` }}>
        <div className="list" ref={list}>
          {messages.length === 0 && <p className="faint">No messages in #{channel.name} yet. Say something below.</p>}
          {messages.map((m, i) => {
            const newDay = i === 0 || day(messages[i - 1].created_at) !== day(m.created_at)
            return (
              <div key={m.id} style={{ display: 'contents' }}>
                {newDay && <div className="daybreak">{day(m.created_at)}</div>}
                <MessageRow m={m} onThread={openThread} />
              </div>
            )
          })}
        </div>
        <div className="composer">
          <form onSubmit={send}>
            <label htmlFor="composer" style={{ position: 'absolute', width: 1, height: 1, overflow: 'hidden', clip: 'rect(0 0 0 0)' }}>Message</label>
            <input id="composer" autoFocus value={draft} onChange={e => setDraft(e.target.value)} placeholder={`Message #${channel.name}`} />
            <button className="primary" disabled={!draft.trim()}>Send</button>
          </form>
          {error && <p className="error">{error}</p>}
        </div>
      </section>
      {panel === 'access' && <AccessPanel channelId={channel.id} channelName={channel.name} onClose={() => setPanel(null)} />}
      {threadId && <ThreadPanel key={threadId} channelId={channel.id} channelName={channel.name} rootId={threadId} onReplied={refresh} onClose={() => setThreadId(null)} />}
      {panel === 'activity' && <ActivityPanel key={version} channelId={channel.id} channelName={channel.name} onClose={() => setPanel(null)} />}
    </main>
  )
}
