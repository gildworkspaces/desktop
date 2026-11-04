'use client'
// Agents: adding one (gild sets it up in the AI tools on this computer),
// what it may do in a channel, and what agents did there.
import { useEffect, useState } from 'react'
import { core, clock, day, errorText, type Agent, type AgentAccess, type Access, type AuditEntry, type Channel, type ClientStatus, type Connected } from '@/lib/core'

export function AgentsSidebar({ channels, current }: { channels: Channel[]; current: Channel | null }) {
  const [agents, setAgents] = useState<Agent[]>([])
  const [name, setName] = useState('')
  const [open, setOpen] = useState<{ agent: Agent; fresh: boolean } | null>(null)
  const [status, setStatus] = useState<{ ok: boolean; text: string } | null>(null)
  const [error, setError] = useState('')

  useEffect(() => {
    core.agents().then(setAgents, e => setError(errorText(e)))
    core.mcpStatus().then(t => setStatus({ ok: true, text: t }), e => setStatus({ ok: false, text: errorText(e) }))
  }, [])

  async function add(e: React.FormEvent) {
    e.preventDefault()
    setError('')
    try {
      const made = await core.createAgent(name)
      setAgents(a => [...a, made.agent])
      setOpen({ agent: made.agent, fresh: true })
      setName('')
    } catch (err) { setError(errorText(err)) }
  }

  return (
    <div className="group">
      <span className="label">Agents</span>
      {agents.length === 0 && <p className="note">No agents yet. Add one and gild sets it up.</p>}
      {agents.map(a => (
        <button key={a.id} className="member agentrow" onClick={() => setOpen({ agent: a, fresh: false })} aria-label={`${a.name}: settings`}>
          <span className="dot agent" aria-hidden="true" />{a.name}
          {a.wake && <span className="badge" title="Wakes when mentioned">wakes on @</span>}
        </button>
      ))}
      <form className="addchan" onSubmit={add}>
        <label htmlFor="new-agent" className="sr">New agent</label>
        <input id="new-agent" className="field" placeholder="+ add agent" value={name} onChange={e => setName(e.target.value)} />
      </form>
      {error && <p className="error" style={{ padding: '0 10px' }}>{error}</p>}
      {status && !status.ok && <p className="error" style={{ padding: '0 10px' }}>{status.text}</p>}
      {open && (
        <AgentSetup
          key={open.agent.id}
          agent={open.agent}
          fresh={open.fresh}
          channels={channels}
          current={current}
          onAgents={setAgents}
          onClose={() => setOpen(null)}
        />
      )}
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
function AgentSetup({ agent, fresh, channels, current, onAgents, onClose }: {
  agent: Agent; fresh: boolean; channels: Channel[]; current: Channel | null; onAgents: (agents: Agent[]) => void; onClose: () => void
}) {
  const [step, setStep] = useState<Step>('connect')
  const [clients, setClients] = useState<ClientStatus[] | null>(null)
  const [chosen, setChosen] = useState<Set<string>>(new Set())
  const [connected, setConnected] = useState<Connected | null>(null)
  const [command, setCommand] = useState(agent.wake ?? '')
  const [context, setContext] = useState(String(agent.context ?? 20))
  const [saved, setSaved] = useState('')
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

  return (
    <div className="modalback top" role="dialog" aria-modal="true" aria-labelledby="setup-title">
      <div className="modal setup">
        <h2 id="setup-title">{fresh ? `Set up ${agent.name}` : agent.name}</h2>
        <ol className="steps" aria-label="Steps">
          {steps.map((s, i) => (
            <li key={s.key} className={s.key === step ? 'on' : i < index ? 'past' : ''}>
              <button type="button" className="link" onClick={() => go(s.key)} disabled={fresh && i > index} aria-current={s.key === step ? 'step' : undefined}>
                <span className="num">{i + 1}</span>{s.label}
              </button>
            </li>
          ))}
        </ol>

        {step === 'connect' && (
          <>
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
            <details className="manual" open={!command || undefined}>
              <summary>Advanced: what gild runs</summary>
              <p className="faint" style={{ fontSize: 12 }}>gild runs this on this computer with the mention and the messages as its input. One run at a time, up to ten minutes; runs and failures show in the channel’s Activity. Empty: {agent.name} isn’t woken.</p>
              <label htmlFor="wake-command" className="sr">Command</label>
              <input id="wake-command" className="field mono" value={command} onChange={e => { setCommand(e.target.value); setSaved('') }} spellCheck={false} placeholder={usable(clients)[0]?.wake ?? 'claude -p --allowedTools mcp__gild-' + agent.name} />
              {usable(clients).filter(c => c.wake !== command).map(c => (
                <div key={c.id}><button type="button" className="link" onClick={() => setCommand(c.wake)}>Use {c.name}’s</button></div>
              ))}
            </details>
            <div className="row"><button className="primary" disabled={busy}>Save</button>{saved && <span className="ok">{saved}</span>}</div>
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
                <button className="ghost" onClick={onClose}>Close</button>
              </>
            )}
          </div>
        )}
        {fresh && step !== 'done' && <div className="row"><span className="spacer" /><button className="ghost" onClick={onClose}>Finish later</button></div>}
      </div>
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
    try { setRows(await core.setAccess(channelId, agentId, (value || null) as Access | null)) } catch (err) { setError(errorText(err)) }
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
