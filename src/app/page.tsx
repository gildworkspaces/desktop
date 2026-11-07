'use client'
import { useCallback, useEffect, useRef, useState } from 'react'
import { ViewPane } from './ChannelView'
import { DocsPane } from './Docs'
import { FilesPane } from './Files'
import { MembersGroup, NameModal, PlusButton, ActivityPanel } from './Agents'
import { AccessPanel } from './Org'
import { MeContext, UserCard, type SettingsSection } from './Profile'
import { SettingsModal } from './Settings'
import { HeldBar, MessageRow, ReachContext, ReachWarning, ThreadPanel, WorkingLine, type Reach } from './Thread'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { core, inApp, onChanged, tint, initial, day, errorText, type Profile, type Workspace, type Channel, type ChannelDocs, type Message } from '@/lib/core'

/** What every screen needs: you, and a way to open Settings. */
type Me = { profile: Profile; setProfile: (p: Profile) => void; settings: (s: SettingsSection) => void }

export default function App() {
  const [ready, setReady] = useState<'checking' | 'app' | 'browser'>('checking')
  const [profile, setProfile] = useState<Profile | null>(null)
  const [open, setOpen] = useState<Workspace | null>(null)
  const [settings, setSettings] = useState<SettingsSection | null>(null)
  // The open workspace's channels, for Settings' agent pages.
  const [place, setPlace] = useState<{ channels: Channel[]; current: Channel | null }>({ channels: [], current: null })
  const [error, setError] = useState('')

  useEffect(() => {
    if (!inApp()) return setReady('browser')
    setReady('app')
    core.profile().then(setProfile, e => setError(errorText(e)))
  }, [])
  // The theme: "system" leaves it to the computer's setting.
  useEffect(() => {
    const root = document.documentElement
    if (!profile || profile.theme === 'system') delete root.dataset.theme
    else root.dataset.theme = profile.theme
  }, [profile])

  if (ready === 'checking') return <main className="center" aria-busy="true" />
  if (ready === 'browser') return <main className="center"><p className="soft">This page is gild's desktop interface. Open it in the gild app.</p></main>
  if (error) return <main className="center"><p className="error">{error}</p></main>
  if (!profile) return <main className="center" aria-busy="true" />
  const me: Me = { profile, setProfile, settings: setSettings }
  return (
    <MeContext.Provider value={profile}>
      {open
        ? <WorkspaceView key={open.id} me={me} workspace={open} onPlace={setPlace} onBack={() => { setOpen(null); setPlace({ channels: [], current: null }) }} />
        : <Home me={me} onOpen={setOpen} />}
      {settings && (
        <SettingsModal
          section={settings}
          profile={profile}
          onProfile={setProfile}
          workspace={open}
          channels={place.channels}
          current={place.current}
          onImported={w => { setSettings(null); setOpen(w) }}
          onClose={() => setSettings(null)}
        />
      )}
    </MeContext.Provider>
  )
}

function Logo({ size = 28 }: { size?: number }) {
  return <img src="/logo.svg" width={size} height={size} alt="" aria-hidden="true" />
}

function Home({ me, onOpen }: { me: Me; onOpen: (w: Workspace) => void }) {
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
        <UserCard profile={me.profile} onProfile={me.setProfile} onSettings={me.settings} placement="down" />
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

function WorkspaceView({ me, workspace, onPlace, onBack }: {
  me: Me; workspace: Workspace; onPlace: (p: { channels: Channel[]; current: Channel | null }) => void; onBack: () => void
}) {
  const [channels, setChannels] = useState<Channel[]>([])
  const [current, setCurrent] = useState<Channel | null>(null)
  const [naming, setNaming] = useState(false)
  const [error, setError] = useState('')

  useEffect(() => {
    core.channels(workspace.id).then(cs => { setChannels(cs); setCurrent(cs[0] ?? null) }, e => setError(errorText(e)))
  }, [workspace.id])
  useEffect(() => { onPlace({ channels, current }) }, [channels, current, onPlace])

  return (
    <div className="ws">
      <nav className="side" aria-label="Channels">
        <div className="sidescroll">
          <button className="back" onClick={onBack}>← All workspaces</button>
          <div className="wsname">
            <div className="tile" style={{ background: tint(workspace.name) }} aria-hidden="true">{initial(workspace.name)}</div>
            <span>{workspace.name}</span>
          </div>
          <div className="group">
            <div className="grouphead"><span className="label">Channels</span><PlusButton label="Create a channel" onClick={() => setNaming(true)} /></div>
            {channels.map(c => (
              <button key={c.id} className={c.id === current?.id ? 'chan on' : 'chan'} onClick={() => setCurrent(c)} aria-current={c.id === current?.id ? 'page' : undefined}>
                <span className="hash" aria-hidden="true">#</span>{c.name}
              </button>
            ))}
            {error && <p className="error" style={{ padding: '0 10px' }}>{error}</p>}
          </div>
          <MembersGroup profile={me.profile} channels={channels} current={current} onOpenAgent={a => me.settings(`agent:${a.id}`)} />
        </div>
        <div className="foot"><UserCard profile={me.profile} onProfile={me.setProfile} onSettings={me.settings} /></div>
      </nav>
      {current ? <ChannelView key={current.id} channel={current} /> : <main className="channel" />}
      {naming && (
        <NameModal
          title="Create a channel"
          label="Name"
          hint="Lower case, with dashes: “Student Intake” becomes #student-intake."
          action="Create channel"
          id="channel-name"
          extra={{ id: 'channel-topic', label: 'Topic (optional)', placeholder: 'What this channel is for' }}
          onSubmit={async (name, topic) => {
            const c = await core.createChannel(workspace.id, name, topic)
            setChannels(cs => [...cs, c])
            setCurrent(c)
            setNaming(false)
          }}
          onClose={() => setNaming(false)}
        />
      )}
    </div>
  )
}

function ChannelView({ channel }: { channel: Channel }) {
  // Which agents a mention here reaches, for colouring and warnings.
  const [reach, setReach] = useState<Reach | null>(null)
  const [reachVersion, setReachVersion] = useState(0)
  useEffect(() => {
    Promise.all([core.channelAccess(channel.id), core.agents()]).then(([rows, agents]) => setReach({
      channelId: channel.id,
      channel: channel.name,
      agents: Object.fromEntries(rows.map(r => [r.agent.name, { id: r.agent.id, in: !!r.access, picture: agents.find(a => a.id === r.agent.id)?.picture }])),
    }), () => setReach(null))
  }, [channel.id, channel.name, reachVersion])
  useEffect(() => {
    const again = () => setReachVersion(v => v + 1)
    window.addEventListener('gild:reach', again)
    window.addEventListener('gild:agents', again)
    return () => { window.removeEventListener('gild:reach', again); window.removeEventListener('gild:agents', again) }
  }, [])
  const [messages, setMessages] = useState<Message[]>([])
  const [draft, setDraft] = useState('')
  const [expanded, setExpanded] = useState(false)
  const [mode, setMode] = useState<'table' | 'board' | 'docs' | 'files'>('table')
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
  const show = (m: 'table' | 'board' | 'docs' | 'files') => { setChosen(true); setMode(m) }
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
    <ReachContext.Provider value={reach}>
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
          <button className={mode === 'files' ? 'tab on' : 'tab'} aria-pressed={mode === 'files'} onClick={() => show('files')}>Files</button>
        </div>
        <button className="ghost" onClick={() => { setThreadId(null); setPanel(p => (p === 'access' ? null : 'access')) }}>Access</button>
        <button className="ghost" onClick={() => { setThreadId(null); setPanel(p => (p === 'activity' ? null : 'activity')) }}>Activity</button>
        <button className="ghost" onClick={() => setExpanded(x => !x)}>{expanded ? 'Collapse view' : 'Expand view'}</button>
      </header>
      <section className="viewpane" aria-label="Channel view" style={{ flex: `${expanded ? 2 : 1} 1 0` }}>
        {mode === 'files'
          ? <FilesPane channelId={channel.id} onEditing={() => setExpanded(true)} />
          : mode === 'docs'
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
        <WorkingLine channelId={channel.id} />
        <HeldBar channelId={channel.id} version={version + messages.length} onContinued={refresh} />
        <div className="composer">
          <form onSubmit={send}>
            <label htmlFor="composer" style={{ position: 'absolute', width: 1, height: 1, overflow: 'hidden', clip: 'rect(0 0 0 0)' }}>Message</label>
            <input id="composer" autoFocus value={draft} onChange={e => setDraft(e.target.value)} placeholder={`Message #${channel.name}`} />
            <button className="primary" disabled={!draft.trim()}>Send</button>
          </form>
          <ReachWarning text={draft} channelId={channel.id} />
          {error && <p className="error">{error}</p>}
        </div>
      </section>
      {panel === 'access' && <AccessPanel channelId={channel.id} channelName={channel.name} workspaceId={channel.workspace_id} onClose={() => setPanel(null)} />}
      {threadId && <ThreadPanel key={threadId} channelId={channel.id} channelName={channel.name} rootId={threadId} onReplied={refresh} onClose={() => setThreadId(null)} />}
      {panel === 'activity' && <ActivityPanel key={version} channelId={channel.id} channelName={channel.name} onClose={() => setPanel(null)} />}
    </main>
    </ReachContext.Provider>
  )
}
