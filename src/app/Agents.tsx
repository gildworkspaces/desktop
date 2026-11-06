'use client'
// Agents: adding one (gild sets it up in the AI tools on this computer),
// the members list, Settings' agents page, what an agent may do in a
// channel, and what agents did there.
import { useEffect, useState } from 'react'
import { core, clock, day, errorText, type Agent, type AgentAccess, type Access, type AuditEntry, type Channel, type ClientStatus, type Connected, type LoopRules, type ModelInfo, type Profile, type ProviderInfo, type ToolInfo } from '@/lib/core'
import { Avatar } from './Profile'

/** Tell every agents list in the window that the agents changed. */
const changed = () => window.dispatchEvent(new Event('gild:agents'))

function useAgents(): [Agent[], (a: Agent[]) => void, string] {
  const [agents, setAgents] = useState<Agent[]>([])
  const [error, setError] = useState('')
  useEffect(() => {
    const load = () => core.agents().then(setAgents, e => setError(errorText(e)))
    load()
    window.addEventListener('gild:agents', load)
    return () => window.removeEventListener('gild:agents', load)
  }, [])
  return [agents, (a: Agent[]) => { setAgents(a); changed() }, error]
}

export function PlusButton({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <button className="plus" onClick={onClick} aria-label={label} title={label}>
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" aria-hidden="true"><path d="M12 5v14M5 12h14" /></svg>
    </button>
  )
}

/** Adding an agent: its name, then the setup steps. */
export function useAddAgent(channels: Channel[], current: Channel | null, onAgents: (a: Agent[]) => void) {
  const [naming, setNaming] = useState(false)
  const [setting, setSetting] = useState<{ agent: Agent; fresh: boolean } | null>(null)
  const ui = (
    <>
      {naming && (
        <NameModal
          title="Add an agent"
          label="Name"
          hint="What people write after @ to ask it something, e.g. intake."
          action="Continue"
          id="agent-name"
          onSubmit={async name => {
            const made = await core.createAgent(name)
            onAgents(await core.agents())
            setNaming(false)
            setSetting({ agent: made.agent, fresh: true })
          }}
          onClose={() => setNaming(false)}
        />
      )}
      {setting && (
        <AgentSetup key={setting.agent.id} agent={setting.agent} fresh={setting.fresh} channels={channels} current={current} onAgents={onAgents} onClose={() => setSetting(null)} />
      )}
    </>
  )
  return { add: () => setNaming(true), open: (agent: Agent) => setSetting({ agent, fresh: false }), ui }
}

/** A small modal asking for one name (and, optionally, more fields given as children). */
export function NameModal({ title, label, hint, action, id, onSubmit, onClose, extra }: {
  title: string; label: string; hint?: string; action: string; id: string
  onSubmit: (name: string, extra: string) => Promise<void>; onClose: () => void; extra?: { id: string; label: string; placeholder: string }
}) {
  const [name, setName] = useState('')
  const [more, setMore] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  useEffect(() => {
    const esc = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose() }
    document.addEventListener('keydown', esc)
    return () => document.removeEventListener('keydown', esc)
  }, [onClose])
  return (
    <div className="modalback top" role="dialog" aria-modal="true" aria-labelledby={`${id}-title`}>
      <form className="modal small" onSubmit={async e => {
        e.preventDefault()
        setBusy(true)
        setError('')
        try { await onSubmit(name, more) } catch (err) { setError(errorText(err)) } finally { setBusy(false) }
      }}>
        <h2 id={`${id}-title`}>{title}</h2>
        <label htmlFor={id} className="flabel">{label}</label>
        <input id={id} className="field" autoFocus value={name} onChange={e => setName(e.target.value)} />
        {hint && <p className="faint" style={{ fontSize: 12 }}>{hint}</p>}
        {extra && (
          <>
            <label htmlFor={extra.id} className="flabel">{extra.label}</label>
            <input id={extra.id} className="field" placeholder={extra.placeholder} value={more} onChange={e => setMore(e.target.value)} />
          </>
        )}
        {error && <p className="error">{error}</p>}
        <div className="row end">
          <button type="button" className="ghost" onClick={onClose}>Cancel</button>
          <button className="primary" disabled={busy || !name.trim()}>{action}</button>
        </div>
      </form>
    </div>
  )
}

/** The sidebar's members: you, then the agents. */
export function MembersGroup({ profile, channels, current, onOpenAgent }: { profile: Profile; channels: Channel[]; current: Channel | null; onOpenAgent: (a: Agent) => void }) {
  const [agents, setAgents, error] = useAgents()
  const [status, setStatus] = useState<string>('')
  useEffect(() => { core.mcpStatus().then(() => setStatus(''), e => setStatus(errorText(e))) }, [])
  const adding = useAddAgent(channels, current, setAgents)
  return (
    <div className="group">
      <div className="grouphead"><span className="label">Members</span><PlusButton label="Add an agent" onClick={adding.add} /></div>
      <div className="member"><Avatar profile={profile} size={20} />{profile.name} <span className="faint">(you)</span></div>
      {agents.map(a => (
        <button key={a.id} className="member agentrow" onClick={() => onOpenAgent(a)} aria-label={`${a.name}: settings`}>
          <span className="aavatar" aria-hidden="true">{a.name[0]?.toUpperCase()}</span>{a.name}
          <span className="tag">{a.coordinator ? 'coordinator' : 'agent'}</span>
        </button>
      ))}
      {error && <p className="error" style={{ padding: '0 10px' }}>{error}</p>}
      {status && <p className="error" style={{ padding: '0 10px' }}>{status}</p>}
      {adding.ui}
    </div>
  )
}

/** Running an agent on a cloud provider: the provider, then its model. */
function CloudSetup({ agent, onAgents, onSaved }: { agent: Agent; onAgents: (a: Agent[]) => void; onSaved: () => void }) {
  const [providers, setProviders] = useState<ProviderInfo[] | null>(null)
  const [provider, setProvider] = useState(agent.runner?.provider ?? 'workers-ai')
  const [models, setModels] = useState<ModelInfo[] | null>(null)
  const [model, setModel] = useState(agent.runner?.model ?? '')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  useEffect(() => { core.providers().then(setProviders, e => setError(errorText(e))) }, [])
  const chosen = providers?.find(p => p.id === provider)
  const ready = !!chosen?.account_id && !!chosen?.has_token
  useEffect(() => {
    if (!ready) return
    setModels(null)
    core.providerModels(provider).then(ms => { setModels(ms); if (!model) setModel(ms.find(m => m.tools)?.id ?? '') }, e => setError(errorText(e)))
  }, [ready, provider]) // eslint-disable-line react-hooks/exhaustive-deps
  const picked = models?.find(m => m.id === model)
  async function save() {
    setBusy(true)
    setError('')
    try { onAgents(await core.setRunner(agent.id, { provider, model })); onSaved() } catch (e) { setError(errorText(e)) } finally { setBusy(false) }
  }
  return (
    <div className="stepform">
      <p className="soft">gild calls the provider’s model when {agent.name} is mentioned, and runs the tools it asks for here, with the same switches and Activity as any agent. The provider never connects to this computer.</p>
      <label htmlFor="provider" className="flabel">Provider</label>
      <select id="provider" className="field" value={provider} onChange={e => { setProvider(e.target.value); setModel('') }}>
        {providers?.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}
      </select>
      {chosen?.vault_error && <p className="error">{chosen.vault_error}</p>}
      {providers && !ready && !chosen?.vault_error && <p className="faint hint">{chosen?.name ?? 'This provider'} isn’t set up yet: add your account ID and API token in Settings › Providers.</p>}
      {ready && (
        <>
          <label htmlFor="model" className="flabel">Model</label>
          {!models && !error && <p className="faint">Loading the models…</p>}
          {models && (
            <select id="model" className="field" value={model} onChange={e => setModel(e.target.value)}>
              <optgroup label="Can use tools">{models.filter(m => m.tools).map(m => <option key={m.id} value={m.id}>{m.id}</option>)}</optgroup>
              <optgroup label="Can only answer (no tools)">{models.filter(m => !m.tools).map(m => <option key={m.id} value={m.id}>{m.id}</option>)}</optgroup>
            </select>
          )}
          {picked && <p className="faint hint">{picked.description}{picked.tools ? '' : ' It can’t use gild’s tools: it can only answer, and gild posts its answer.'}</p>}
          <div className="row">
            <button className="primary" onClick={save} disabled={busy || !model}>{busy ? 'Saving…' : agent.runner ? 'Save' : 'Run in the cloud'}</button>
            {agent.runner && <button className="ghost" onClick={async () => { setBusy(true); try { onAgents(await core.setRunner(agent.id, null)); onSaved() } catch (e) { setError(errorText(e)) } finally { setBusy(false) } }}>Run on this computer instead</button>}
          </div>
        </>
      )}
      {error && <p className="error">{error}</p>}
    </div>
  )
}

/** A labelled on/off switch. */
export function Toggle({ id, label, hint, on, onChange, disabled }: { id: string; label: string; hint?: string; on: boolean; onChange: (on: boolean) => void; disabled?: boolean }) {
  return (
    <div className="toggle">
      <span className="col">
        <label htmlFor={id}>{label}</label>
        {hint && <span className="faint hint">{hint}</span>}
      </span>
      <button id={id} role="switch" aria-checked={on} className={on ? 'switch on' : 'switch'} disabled={disabled} onClick={() => onChange(!on)}><span /></button>
    </div>
  )
}

/** Settings › Agents › Runaway loops. */
function LoopSettings() {
  const [rules, setRules] = useState<LoopRules | null>(null)
  const [limit, setLimit] = useState('')
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null)
  useEffect(() => { core.loopRules().then(r => { setRules(r); setLimit(String(r.limit)) }, e => setNote({ ok: false, text: errorText(e) })) }, [])
  async function save(next: LoopRules) {
    setNote(null)
    try { const r = await core.setLoopRules(next); setRules(r); setLimit(String(r.limit)); setNote({ ok: true, text: 'Saved.' }) } catch (e) { setNote({ ok: false, text: errorText(e) }) }
  }
  if (!rules) return note ? <p className="error">{note.text}</p> : null
  return (
    <div className="sfield ruled loops">
      <span className="flabel">Runaway loops</span>
      <form className="ctxrow" onSubmit={e => { e.preventDefault(); save({ ...rules, limit: Number(limit) }) }}>
        <label htmlFor="agent-limit">Pause agents after</label>
        <input id="agent-limit" className="field num" type="number" min={1} max={10000} value={limit} onChange={e => setLimit(e.target.value)} />
        <span>agent messages in a row</span>
        {limit !== String(rules.limit) && <button className="primary">Save</button>}
      </form>
      <span className="faint hint">Counted in each channel and thread since a person or a coordinator last wrote there. At the limit, gild holds the agents (nothing is lost) and says so in the channel; a reply from a person or a coordinator lets them continue.</span>
      <Toggle id="ask-coordinator" label="Ask the coordinator" hint="When the channel has a coordinator agent, gild @mentions it so it can decide whether to continue." on={rules.ask_coordinator} onChange={v => save({ ...rules, ask_coordinator: v })} />
      <Toggle id="notify-me" label="Notify me" hint="A notification on this computer when agents are held." on={rules.notify_me} onChange={v => save({ ...rules, notify_me: v })} />
      {note && <span className={note.ok ? 'ok' : 'error'}>{note.text}</span>}
    </div>
  )
}

/** Settings › Agents. */
export function AgentsSettings({ channels, current, onOpenAgent }: { channels: Channel[]; current: Channel | null; onOpenAgent: (a: Agent) => void }) {
  const [agents, setAgents, error] = useAgents()
  const [endpoint, setEndpoint] = useState<{ ok: boolean; text: string } | null>(null)
  const [where, setWhere] = useState<Record<string, number>>({})
  useEffect(() => { core.mcpStatus().then(t => setEndpoint({ ok: true, text: t }), e => setEndpoint({ ok: false, text: errorText(e) })) }, [])
  useEffect(() => {
    Promise.all(channels.map(c => core.channelAccess(c.id))).then(all => {
      const n: Record<string, number> = {}
      for (const rows of all) for (const r of rows) if (r.access) n[r.agent.id] = (n[r.agent.id] ?? 0) + 1
      setWhere(n)
    }, () => {})
  }, [channels, agents])
  const adding = useAddAgent(channels, current, setAgents)
  return (
    <div className="spage">
      <p className="soft lead">Agents work in the channels you share with them. gild starts one when it’s mentioned, or when someone writes in a thread it’s in, and every call it makes is in the channel’s Activity.</p>
      {agents.length > 0 && (
        <div className="srows">
          {agents.map(a => (
            <div key={a.id} className="srow agentline">
              <span className="aname"><span className="aavatar big" aria-hidden="true">{a.name[0]?.toUpperCase()}</span><b>{a.name}</b>{a.coordinator && <span className="tag">coordinator</span>}</span>
              <span className="soft">{channels.length > 0 ? `${where[a.id] ?? 0} ${(where[a.id] ?? 0) === 1 ? 'channel' : 'channels'} here` : ''}</span>
              <span className="soft">{a.runner ? `cloud · ${a.runner.model.split('/').pop()}` : a.wake ? 'wakes on @' : 'not woken'}</span>
              <button className="ghost" onClick={() => onOpenAgent(a)} aria-label={`${a.name}: settings`}>Settings</button>
            </div>
          ))}
        </div>
      )}
      {agents.length === 0 && <p className="faint">No agents yet.</p>}
      <div><button className="primary" onClick={adding.add}>Add an agent</button></div>
      {error && <p className="error">{error}</p>}
      <LoopSettings />
      {endpoint && (
        <div className="sfield ruled">
          <span className="flabel">Agent endpoint</span>
          {endpoint.ok
            ? <><span className="mono endpoint"><span className="sdot online" aria-hidden="true" />{endpoint.text}</span><span className="faint" style={{ fontSize: 12 }}>Running. It only answers on this computer.</span></>
            : <p className="error">{endpoint.text}</p>}
        </div>
      )}
      {adding.ui}
    </div>
  )
}

type Step = 'connect' | 'channels' | 'mentions' | 'done'
// Setting up: connect (which also sets what wakes it), channels, and how to
// use it. Settings: the same, plus what comes with a mention.
const SETUP: { key: Step; label: string }[] = [
  { key: 'connect', label: 'Connect' },
  { key: 'channels', label: 'Channels' },
  { key: 'done', label: 'Done' },
]
const SETTINGS: { key: Step; label: string }[] = [
  { key: 'connect', label: 'Connect' },
  { key: 'channels', label: 'Channels' },
  { key: 'mentions', label: 'Mentions' },
]

/** Setting an agent up: gild adds it to the AI tools on this computer (and
 *  wakes it through the first of them), shares channels with it, and says
 *  how to use it. Opened again from the sidebar, it's the agent's settings. */
export function AgentSetup({ agent, fresh, channels, current, onAgents, onClose, page }: {
  agent: Agent; fresh: boolean; channels: Channel[]; current: Channel | null; onAgents: (agents: Agent[]) => void; onClose: () => void
  /** Shown as one tab of the agent's settings page, not as a dialog. */
  page?: Exclude<Step, 'done'>
}) {
  const [ownStep, setStep] = useState<Step>(page ?? 'connect')
  // Where the agent's model runs: this computer (the default) or a cloud provider.
  const [runsOn, setRunsOn] = useState<'here' | 'cloud'>(agent.runner ? 'cloud' : 'here')
  const step = page ?? ownStep
  const [clients, setClients] = useState<ClientStatus[] | null>(null)
  const [chosen, setChosen] = useState<Set<string>>(new Set())
  const [connected, setConnected] = useState<Connected | null>(null)
  const [command, setCommand] = useState(agent.wake ?? '')
  const [context, setContext] = useState(String(agent.context ?? 20))
  const [saved, setSaved] = useState('')
  // Open once if there's no command yet; never closed by typing into it.
  const [advancedOpen] = useState(!agent.wake)
  const [coordinator, setCoordinator] = useState(!!agent.coordinator)
  const [shared, setShared] = useState<Map<string, Access | null> | null>(null)
  const [picked, setPicked] = useState<Set<string>>(new Set())
  const [busy, setBusy] = useState(false)
  const [removing, setRemoving] = useState(false)
  const [error, setError] = useState('')

  useEffect(() => {
    core.agentClients(agent.id).then(cs => {
      setClients(cs)
      setChosen(new Set(cs.filter(c => c.installed).map(c => c.id)))
    }, e => setError(errorText(e)))
    Promise.all(channels.map(c => core.channelAccess(c.id).then(rows => [c.id, rows.find(r => r.agent.id === agent.id)?.access ?? null] as const)))
      .then(pairs => {
        const m = new Map(pairs)
        setShared(m)
        const on = new Set(pairs.filter(([, a]) => a).map(([id]) => id))
        if (fresh && current) on.add(current.id)
        setPicked(on)
      }, e => setError(errorText(e)))
  }, [agent.id, channels, current, fresh])

  const usable = (cs: ClientStatus[] | null) => cs?.filter(c => c.connected) ?? []

  async function act(f: () => Promise<void>) {
    setBusy(true)
    setError('')
    try { await f() } catch (e) { setError(errorText(e)) } finally { setBusy(false) }
  }

  const connect = () => act(async () => {
    const made = await core.connectAgent(agent.id, [...chosen])
    setConnected(made)
    setClients(made.clients)
    onAgents(made.agents)
    setCommand(made.agents.find(a => a.id === agent.id)?.wake ?? '')
    if (made.errors.length === 0 && chosen.size > 0) setStep('channels')
  })

  const saveMentions = (wake: string) => act(async () => {
    const n = Number(context)
    if (!Number.isInteger(n) || n < 0 || n > 200) throw new Error('Between 0 and 200 messages.')
    await core.setContext(agent.id, n)
    onAgents(await core.setWake(agent.id, wake))
    setCommand(wake)
    setSaved('Saved.')
  })

  const share = () => act(async () => {
    for (const c of channels) {
      const had = shared?.get(c.id) ?? null
      const want = picked.has(c.id)
      // Checked means it can answer there, which takes write access; a
      // channel already shared read-only stays read-only.
      if (want && !had) await core.setAccess(c.id, agent.id, 'write')
      if (!want && had) await core.setAccess(c.id, agent.id, null)
    }
    setShared(new Map(channels.map(c => [c.id, picked.has(c.id) ? (shared?.get(c.id) ?? 'write') : null])))
    if (fresh) setStep('done')
    else setSaved('Saved.')
  })

  const remove = () => act(async () => {
    onAgents(await core.removeAgent(agent.id))
    onClose()
  })

  const where = channels.filter(c => picked.has(c.id))
  const here = where.find(c => c.id === current?.id) ?? where[0]
  const installed = clients?.filter(c => c.installed) ?? []
  const steps = fresh ? SETUP : SETTINGS
  const index = steps.findIndex(s => s.key === step)
  const go = (s: Step) => { setSaved(''); setError(''); setStep(s) }

  const body = (
      <>
        {!page && <h2 id="setup-title">{fresh ? `Set up ${agent.name}` : agent.name}</h2>}
        {!page && <ol className="steps" aria-label="Steps">
          {steps.map((s, i) => (
            <li key={s.key} className={s.key === step ? 'on' : i < index ? 'past' : ''}>
              <button type="button" className="link" onClick={() => go(s.key)} disabled={fresh && i > index} aria-current={s.key === step ? 'step' : undefined}>
                <span className="num">{i + 1}</span>{s.label}
              </button>
            </li>
          ))}
        </ol>}

        {step === 'connect' && (
          <>
            <div className="seg" role="radiogroup" aria-label="Where it runs">
              <button role="radio" aria-checked={runsOn === 'here'} className={runsOn === 'here' ? 'on' : ''} onClick={() => setRunsOn('here')}>This computer</button>
              <button role="radio" aria-checked={runsOn === 'cloud'} className={runsOn === 'cloud' ? 'on' : ''} onClick={() => setRunsOn('cloud')}>Cloud</button>
            </div>
            {runsOn === 'cloud' ? <CloudSetup agent={agent} onAgents={onAgents} onSaved={() => { if (fresh) go('channels'); else setSaved('Saved.') }} /> : <>
            <p className="soft">gild adds {agent.name} to the AI tools on this computer, so they can work in the channels you share. Nothing to copy or paste.</p>
            {!clients && <p className="faint">Looking for AI tools on this computer…</p>}
            {clients && (
              <div className="clients">
                {clients.map(c => (
                  <label key={c.id} className={c.installed ? 'clientrow' : 'clientrow off'}>
                    <input
                      type="checkbox"
                      disabled={!c.installed || busy}
                      checked={chosen.has(c.id)}
                      onChange={e => setChosen(s => { const n = new Set(s); if (e.target.checked) n.add(c.id); else n.delete(c.id); return n })}
                    />
                    <span className="clientname">{c.name}</span>
                    <span className="faint">{!c.installed ? 'not on this computer' : c.connected ? `has ${agent.name}` : 'found'}</span>
                  </label>
                ))}
              </div>
            )}
            {clients && installed.length === 0 && (
              <p className="faint" style={{ fontSize: 13 }}>No AI tool gild can set up was found. Install Claude Code or Codex and open this again, or connect another tool by hand below.</p>
            )}
            {connected && connected.errors.map(e => <p key={e} className="error">{e}</p>)}
            {connected && connected.errors.length === 0 && usable(connected.clients).length > 0 && (
              <p className="ok">{agent.name} is set up in {usable(connected.clients).map(c => c.name).join(' and ')}.</p>
            )}
            <div className="row">
              {installed.length > 0 && (
                <button className="primary" onClick={connect} disabled={busy || chosen.size === 0}>
                  {busy ? 'Setting up…' : usable(clients).length > 0 ? 'Set up again' : `Set up ${agent.name}`}
                </button>
              )}
              {(usable(clients).length > 0 || connected) && <button className="ghost" onClick={() => go('channels')}>Next</button>}
            </div>
            <details className="manual">
              <summary>Another MCP client</summary>
              {connected ? (
                <>
                  <p className="faint" style={{ fontSize: 12 }}>Streamable HTTP at <code>{connected.endpoint}</code>, header <code>Authorization: Bearer &lt;token&gt;</code>. The endpoint only answers on this computer. This token replaces any earlier one.</p>
                  <div className="secret"><code>{connected.token}</code></div>
                  <p className="faint" style={{ fontSize: 12 }}>To have gild wake that client when {agent.name} is mentioned, set its command in {agent.name}’s settings, under Mentions.</p>
                </>
              ) : (
                <>
                  <p className="faint" style={{ fontSize: 12 }}>gild makes {agent.name} a new token for it. Tools gild set up get the new token too.</p>
                  <div><button className="ghost" onClick={() => act(async () => { const made = await core.connectAgent(agent.id, usable(clients).map(c => c.id)); setConnected(made); setClients(made.clients); onAgents(made.agents) })} disabled={busy}>Show a token</button></div>
                </>
              )}
            </details>
          </>}
          </>
        )}

        {step === 'channels' && (
          <>
            <p className="soft">Which channels can {agent.name} work in? It sees nothing else, and every change here is saved in the channel’s history.</p>
            <div className="clients">
              {channels.map(c => (
                <label key={c.id} className="clientrow">
                  <input type="checkbox" checked={picked.has(c.id)} disabled={!shared || busy}
                    onChange={e => setPicked(s => { const n = new Set(s); if (e.target.checked) n.add(c.id); else n.delete(c.id); return n })} />
                  <span className="clientname">#{c.name}</span>
                  {shared?.get(c.id) === 'read' && <span className="faint">can read only</span>}
                </label>
              ))}
            </div>
            <p className="faint" style={{ fontSize: 12 }}>A checked channel lets {agent.name} read, answer and change it. For read-only, use the channel’s Access button.</p>
            <div className="row"><button className="primary" onClick={share} disabled={busy || !shared}>Save</button>{saved && <span className="ok">{saved}</span>}</div>
          </>
        )}

        {step === 'mentions' && (
          <form onSubmit={e => { e.preventDefault(); saveMentions(command) }} className="stepform">
            <p className="soft">When someone writes <span className="mention">@{agent.name}</span>, gild wakes {agent.name} with the conversation so far.</p>
            <label className="ctxrow" htmlFor="context">
              Send the last
              <input id="context" className="field num" type="number" min={0} max={200} value={context} onChange={e => { setContext(e.target.value); setSaved('') }} />
              messages of the channel with the mention
            </label>
            <details className="manual" open={advancedOpen}>
              <summary>Advanced: what gild runs</summary>
              <p className="faint" style={{ fontSize: 12 }}>gild runs this on this computer with the mention and the messages as its input. One run at a time, up to ten minutes; runs and failures show in the channel’s Activity. Empty: {agent.name} isn’t woken.</p>
              <label htmlFor="wake-command" className="sr">Command</label>
              <input id="wake-command" className="field mono" value={command} onChange={e => { setCommand(e.target.value); setSaved('') }} spellCheck={false} placeholder={usable(clients)[0]?.wake ?? 'claude -p --allowedTools mcp__gild-' + agent.name} />
              {usable(clients).filter(c => c.wake !== command).map(c => (
                <div key={c.id}><button type="button" className="link" onClick={() => setCommand(c.wake)}>Use {c.name}’s</button></div>
              ))}
            </details>
            <div className="row"><button className="primary" disabled={busy}>Save</button>{saved && <span className="ok">{saved}</span>}</div>
            <Toggle
              id="coordinator"
              label="Coordinator"
              hint={`${agent.name} directs other agents: its messages count like a person’s, so they reset the runaway-loop count and reach agents in its threads without an @mention.`}
              on={!!coordinator}
              disabled={busy}
              onChange={on => act(async () => { const list = await core.setCoordinator(agent.id, on); onAgents(list); setCoordinator(on) })}
            />
          </form>
        )}

        {step === 'done' && (
          <>
            {here && command ? (
              <p className="big">Now write <span className="mention">@{agent.name}</span> in #{here.name} and you’re set.</p>
            ) : (
              <p className="soft">
                {!command ? `${agent.name} isn’t set up in an AI tool on this computer, so a mention can’t wake it yet. Set what to run in its settings, under Mentions.` : ''}
                {!here ? ` ${agent.name} can’t see any channel yet (step 2).` : ''}
              </p>
            )}
            <div className="row"><button className="primary" onClick={onClose}>Done</button></div>
          </>
        )}

        {error && <p className="error">{error}</p>}
        {!fresh && step !== 'done' && (
          <div className="row removebar">
            {removing ? (
              <>
                <p className="soft confirm">Remove {agent.name}? It loses every channel{installed.some(c => c.connected) ? ` and is taken out of ${installed.filter(c => c.connected).map(c => c.name).join(' and ')}` : ''}. What it did stays in each channel’s activity.</p>
                <button className="danger" onClick={remove} disabled={busy}>Remove {agent.name}</button>
                <button className="ghost" onClick={() => setRemoving(false)}>Keep it</button>
              </>
            ) : (
              <>
                <button className="ghost" onClick={() => setRemoving(true)}>Remove agent</button>
                <span className="spacer" />
                {!page && <button className="ghost" onClick={onClose}>Close</button>}
              </>
            )}
          </div>
        )}
        {fresh && step !== 'done' && <div className="row"><span className="spacer" /><button className="ghost" onClick={onClose}>Finish later</button></div>}
      </>
  )
  if (page) return <div className="setup page">{body}</div>
  return (
    <div className="modalback top" role="dialog" aria-modal="true" aria-labelledby="setup-title">
      <div className="modal setup">{body}</div>
    </div>
  )
}

type AgentTab = 'tools' | 'channels' | 'mentions' | 'connect'
const AGENT_TABS: { key: AgentTab; label: string }[] = [
  { key: 'tools', label: 'Tools' },
  { key: 'channels', label: 'Channels' },
  { key: 'mentions', label: 'Mentions' },
  { key: 'connect', label: 'Connect' },
]

/** Settings › Agents › an agent: what it may use, where, and how it's woken. */
export function AgentPage({ agentId, channels, current, onBack }: { agentId: string; channels: Channel[]; current: Channel | null; onBack: () => void }) {
  const [agents, setAgents] = useAgents()
  const [tab, setTab] = useState<AgentTab>('tools')
  const agent = agents.find(a => a.id === agentId)
  if (!agent) return <div className="spage"><button className="link back" onClick={onBack}>← Agents</button><p className="faint">Loading…</p></div>
  return (
    <div className="spage">
      <button className="link back" onClick={onBack}>← Agents</button>
      <div className="row"><span className="aavatar big" aria-hidden="true">{agent.name[0]?.toUpperCase()}</span><h2 style={{ fontSize: 20 }}>{agent.name}</h2>{agent.coordinator && <span className="tag">coordinator</span>}</div>
      <div className="seg" role="tablist" aria-label={`${agent.name} settings`}>
        {AGENT_TABS.map(t => <button key={t.key} role="tab" aria-selected={tab === t.key} className={tab === t.key ? 'on' : ''} onClick={() => setTab(t.key)}>{t.label}</button>)}
      </div>
      {tab === 'tools'
        ? <ToolsTab agent={agent} />
        : <AgentSetup key={tab} agent={agent} fresh={false} page={tab} channels={channels} current={current} onAgents={setAgents} onClose={onBack} />}
    </div>
  )
}

/** One agent's tools, on or off, by group. */
function ToolsTab({ agent }: { agent: Agent }) {
  const [catalog, setCatalog] = useState<ToolInfo[] | null>(null)
  const [off, setOff] = useState<string[]>([])
  const [error, setError] = useState('')
  useEffect(() => {
    core.toolCatalog().then(setCatalog, e => setError(errorText(e)))
    core.agentToolsOff(agent.id).then(setOff, e => setError(errorText(e)))
  }, [agent.id])
  async function set(tools: string[], allowed: boolean) {
    setError('')
    try { let last = off; for (const t of tools) last = await core.setAgentTool(agent.id, t, allowed); setOff(last) } catch (e) { setError(errorText(e)) }
  }
  if (!catalog) return error ? <p className="error">{error}</p> : null
  const groups = [...new Set(catalog.map(t => t.group))]
  return (
    <div className="toolgroups">
      <p className="soft lead">{agent.name} can use the tools that are on. A tool that’s off isn’t offered to it, and if it calls it anyway, gild refuses and says so in Activity. Access to each channel is set under Channels.</p>
      {groups.map(g => {
        const tools = catalog.filter(t => t.group === g)
        const allOn = tools.every(t => !off.includes(t.name))
        return (
          <section key={g} className="toolgroup" aria-label={g}>
            <div className="row"><span className="flabel">{g}</span><span className="spacer" /><button className="link" onClick={() => set(tools.map(t => t.name), !allOn)}>{allOn ? 'Turn all off' : 'Turn all on'}</button></div>
            {tools.map(t => (
              <Toggle key={t.name} id={`tool-${t.name}`} label={t.name} hint={t.description.split('. ')[0].replace(/\.$/, '') + '.'} on={!off.includes(t.name)} onChange={v => set([t.name], v)} />
            ))}
          </section>
        )
      })}
      {error && <p className="error">{error}</p>}
    </div>
  )
}

const LEVELS: { value: '' | Access; label: string }[] = [
  { value: '', label: 'No access' },
  { value: 'read', label: 'Can read' },
  { value: 'write', label: 'Can read and change' },
]

export function AccessPanel({ channelId, channelName, onClose }: { channelId: string; channelName: string; onClose: () => void }) {
  const [rows, setRows] = useState<AgentAccess[] | null>(null)
  const [error, setError] = useState('')
  useEffect(() => { core.channelAccess(channelId).then(setRows, e => setError(errorText(e))) }, [channelId])
  async function change(agentId: string, value: string) {
    setError('')
    try { setRows(await core.setAccess(channelId, agentId, (value || null) as Access | null)); window.dispatchEvent(new Event('gild:reach')) } catch (err) { setError(errorText(err)) }
  }
  return (
    <aside className="panel" aria-label={`Access to #${channelName}`}>
      <div className="panelhead"><span className="label">Access to #{channelName}</span><button className="ghost" onClick={onClose}>Close</button></div>
      <div className="panelform">
        <p className="soft" style={{ margin: 0, fontSize: 13 }}>People in the workspace can use every channel. Agents only see the channels shared with them, and every change here is saved in the channel's history.</p>
        {rows?.length === 0 && <p className="faint">No agents yet. Add one in the sidebar.</p>}
        {rows?.map(r => (
          <div key={r.agent.id} className="accessrow">
            <label htmlFor={`acc-${r.agent.id}`}><span className="dot agent" aria-hidden="true" /> {r.agent.name}</label>
            <select id={`acc-${r.agent.id}`} className="field" value={r.access ?? ''} onChange={e => change(r.agent.id, e.target.value)}>
              {LEVELS.map(l => <option key={l.value} value={l.value}>{l.label}</option>)}
            </select>
          </div>
        ))}
        {error && <p className="error">{error}</p>}
      </div>
    </aside>
  )
}

const ACTIONS: Record<string, string> = {
  list_channels: 'listed its channels',
  read_channel: 'read the channel',
  read_thread: 'read a thread',
  post_message: 'posted',
  add_record: 'added',
  update_record: 'changed a record',
  record_history: 'read a record’s history',
  read_doc: 'read a doc',
  search_docs: 'searched the docs',
  propose_doc_edit: 'proposed a change',
  wait_for_mentions: 'picked up a mention',
  woken: 'was woken',
}

export function ActivityPanel({ channelId, channelName, onClose }: { channelId: string; channelName: string; onClose: () => void }) {
  const [rows, setRows] = useState<AuditEntry[] | null>(null)
  const [error, setError] = useState('')
  useEffect(() => { core.activity(channelId).then(setRows, e => setError(errorText(e))) }, [channelId])
  return (
    <aside className="panel" aria-label={`Agent activity in #${channelName}`}>
      <div className="panelhead"><span className="label">Agent activity in #{channelName}</span><button className="ghost" onClick={onClose}>Close</button></div>
      <div className="history" style={{ borderTop: 0 }}>
        {error && <p className="error">{error}</p>}
        {rows?.length === 0 && <p className="faint">No agent has used this channel yet.</p>}
        {rows?.map((a, i) => (
          <div key={i} className="change">
            <span>{a.ok ? '' : '✕ refused: '}<b>{a.agent}</b> {ACTIONS[a.action] ?? a.action}{a.detail ? `: ${a.detail}` : ''}</span>
            <span className="faint mono" style={{ fontSize: 11 }}>{day(a.at)} {clock(a.at)}</span>
          </div>
        ))}
      </div>
    </aside>
  )
}
