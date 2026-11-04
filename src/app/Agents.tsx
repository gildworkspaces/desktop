'use client'
// Agents: adding one (its token and how to connect it), what it may do in a
// channel, and what agents did there.
import { useEffect, useState } from 'react'
import { core, clock, day, errorText, type Agent, type AgentAccess, type Access, type AuditEntry, type NewAgent } from '@/lib/core'

export function AgentsSidebar({ onAdded }: { onAdded?: () => void }) {
  const [agents, setAgents] = useState<Agent[]>([])
  const [name, setName] = useState('')
  const [created, setCreated] = useState<NewAgent | null>(null)
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
      setCreated(made)
      setName('')
      onAdded?.()
    } catch (err) { setError(errorText(err)) }
  }

  return (
    <div className="group">
      <span className="label">Agents</span>
      {agents.length === 0 && <p className="note">No agents yet. Add one, then share channels with it.</p>}
      {agents.map(a => <div key={a.id} className="member"><span className="dot agent" aria-hidden="true" />{a.name}</div>)}
      <form className="addchan" onSubmit={add}>
        <label htmlFor="new-agent" className="sr">New agent</label>
        <input id="new-agent" className="field" placeholder="+ add agent" value={name} onChange={e => setName(e.target.value)} />
      </form>
      {error && <p className="error" style={{ padding: '0 10px' }}>{error}</p>}
      {status && !status.ok && <p className="error" style={{ padding: '0 10px' }}>{status.text}</p>}
      {created && <ConnectAgent made={created} onClose={() => setCreated(null)} />}
    </div>
  )
}

function ConnectAgent({ made, onClose }: { made: NewAgent; onClose: () => void }) {
  const command = `claude mcp add --transport http gild-${made.agent.name} ${made.endpoint} --header "Authorization: Bearer ${made.token}"`
  const [copied, setCopied] = useState('')
  async function copy(what: string, text: string) {
    try { await navigator.clipboard.writeText(text); setCopied(what) } catch { setCopied('') }
  }
  return (
    <div className="modalback" role="dialog" aria-modal="true" aria-labelledby="connect-title">
      <div className="modal">
        <h2 id="connect-title">Connect {made.agent.name}</h2>
        <p className="soft">This is {made.agent.name}'s token. It's shown once: gild keeps only a fingerprint of it. {made.agent.name} sees no channels until you share them, from a channel's <b>Access</b> button.</p>
        <span className="label">Token</span>
        <div className="secret"><code>{made.token}</code><button className="ghost" onClick={() => copy('token', made.token)}>{copied === 'token' ? 'Copied' : 'Copy'}</button></div>
        <span className="label">Claude Code</span>
        <div className="secret"><code>{command}</code><button className="ghost" onClick={() => copy('command', command)}>{copied === 'command' ? 'Copied' : 'Copy'}</button></div>
        <p className="faint" style={{ fontSize: 12 }}>Any MCP client works: Streamable HTTP at {made.endpoint}, header <code>Authorization: Bearer &lt;token&gt;</code>. The endpoint only answers on this computer.</p>
        <div><button className="primary" onClick={onClose}>Done</button></div>
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
  post_message: 'posted',
  add_record: 'added',
  update_record: 'changed a record',
  record_history: 'read a record’s history',
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
