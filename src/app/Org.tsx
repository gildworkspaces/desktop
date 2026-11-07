'use client'
// The org: who works with whom, as an outline of seats (Settings › Org),
// and the org's part of a channel's access.
import { useEffect, useState } from 'react'
import { core, errorText, type Access, type Agent, type AgentAccess, type Channel, type ChannelRules, type Holder, type Org, type Seat } from '@/lib/core'
import { AgentAvatar, NameModal, Toggle } from './Agents'
import { Avatar, useMe } from './Profile'

/** Tell every org view in the window that the org changed. */
export const orgChanged = () => window.dispatchEvent(new Event('gild:org'))

/** Every org, kept current. */
export function useOrgs(): [Org[] | null, (o: Org[]) => void, string] {
  const [orgs, setOrgs] = useState<Org[] | null>(null)
  const [error, setError] = useState('')
  useEffect(() => {
    const load = () => core.orgs().then(setOrgs, e => setError(errorText(e)))
    load()
    window.addEventListener('gild:org', load)
    window.addEventListener('gild:agents', load)
    return () => { window.removeEventListener('gild:org', load); window.removeEventListener('gild:agents', load) }
  }, [])
  return [orgs, (o: Org[]) => { setOrgs(o); orgChanged() }, error]
}

/** The holder's name, as the window shows it. */
export function holderName(h: Holder | null, agents: Agent[], me: { id: string; name: string } | null): string | null {
  if (!h) return null
  if (h.kind === 'person') return h.id === me?.id ? me.name : null
  return agents.find(a => a.id === h.id)?.name ?? null
}

/** A seat's picture: yours, its agent's, or a dashed circle when it's open. */
export function SeatAvatar({ seat, agents, size = 26 }: { seat: Seat; agents: Agent[]; size?: number }) {
  const me = useMe()
  if (seat.holder?.kind === 'person' && me) return <Avatar profile={me} size={size} />
  const agent = seat.holder?.kind === 'agent' ? agents.find(a => a.id === seat.holder?.id) : undefined
  if (agent) return <span className="seatav" style={{ width: size, height: size }}><AgentAvatar agent={agent} big={size > 24} /></span>
  return <span className="seatav open" style={{ width: size, height: size }} aria-hidden="true">?</span>
}

/** Settings › Org: the workspace's org as an outline, and the selected seat. */
export function OrgPage({ workspaceId, channels }: { workspaceId: string | null; channels: Channel[] }) {
  const [orgs, setOrgs, error] = useOrgs()
  const [agents, setAgents] = useState<Agent[]>([])
  const [meId, setMeId] = useState<string | null>(null)
  const [selected, setSelected] = useState<string | null>(null)
  const [adding, setAdding] = useState<Seat | null>(null)
  const [renaming, setRenaming] = useState(false)
  const me = useMe()
  useEffect(() => {
    const load = () => core.agents().then(setAgents, () => {})
    load()
    core.meId().then(setMeId, () => {})
    window.addEventListener('gild:agents', load)
    return () => window.removeEventListener('gild:agents', load)
  }, [])
  const org = orgs?.find(o => workspaceId && o.workspaces.includes(workspaceId)) ?? orgs?.[0]
  if (!org) return <div className="spage">{error ? <p className="error">{error}</p> : <p className="faint">Loading…</p>}</div>
  const seat = org.seats.find(s => s.id === selected) ?? org.seats[0]
  const who = { id: meId ?? '', name: me?.name ?? 'You' }
  const open = org.seats.filter(s => !s.holder).length

  function renderSeat(s: Seat) {
    const kids = org!.seats.filter(k => k.parent === s.id)
    const name = holderName(s.holder, agents, who)
    const agent = s.holder?.kind === 'agent' ? agents.find(a => a.id === s.holder?.id) : undefined
    return (
      <div key={s.id} className="seatgroup">
        <button className={s.id === seat.id ? 'seat sel' : 'seat'} onClick={() => setSelected(s.id)} aria-current={s.id === seat.id ? 'true' : undefined} aria-label={`${s.title}${name ? `, ${name}` : ', open'}`}>
          <SeatAvatar seat={s} agents={agents} />
          <span className="who">
            <span className="title">{s.title}</span>
            {name
              ? <span className="holder">{s.holder?.kind === 'person' && s.holder.id === meId ? `${name} (you)` : name}{agent?.coordinator ? ' · coordinator' : ''}</span>
              : <span className="holder open">Open: waiting for someone to join</span>}
          </span>
          <span className="meta"><span className="opill">L{s.level}</span>{s.team > 1 ? `team of ${s.team - 1}` : ''}</span>
        </button>
        {(kids.length > 0 || s.level < org!.max_levels) && (
          <div className="kids">
            {kids.map(renderSeat)}
            {kids.length > 0 && kids[0].level >= org!.max_levels && <p className="floor">Level {org!.max_levels}: these seats can’t have reports.</p>}
            {s.level < org!.max_levels && (
              <button className="addseat" onClick={() => setAdding(s)}><span className="plusbox" aria-hidden="true">+</span>Add a report to {s.title}</button>
            )}
          </div>
        )}
      </div>
    )
  }

  return (
    <div className="spage">
      <div className="row">
        <h2 style={{ fontSize: 18 }}>{org.name}</h2>
        <span className="opill">{org.seats.length} {org.seats.length === 1 ? 'seat' : 'seats'}{open ? ` · ${open} open` : ''}</span>
        <span className="spacer" />
        <button className="ghost" onClick={() => setRenaming(true)}>Rename</button>
      </div>
      <p className="soft lead">Each seat reports to one other seat, up to {org.max_levels} levels deep. A seat and everyone under it is a team, and a channel can be opened to a whole team in its Access. An open seat keeps its place until someone takes it.</p>
      <div className="orgbody">
        <div className="tree" aria-label={`${org.name}: seats`}>{org.seats[0] && renderSeat(org.seats[0])}</div>
        <SeatPanel key={seat.id} org={org} seat={seat} agents={agents} meId={meId} meName={who.name} channels={channels} onOrgs={setOrgs} onRemoved={() => setSelected(seat.parent)} />
      </div>
      {adding && (
        <NameModal title={`A report to ${adding.title}`} label="Title" id="seat-title" action="Add seat" hint="It starts open. Pick who holds it next."
          onClose={() => setAdding(null)}
          onSubmit={async title => {
            const before = new Set(org.seats.map(s => s.id))
            const next = await core.addSeat(adding.id, title)
            setOrgs(next)
            const made = next.find(o => o.id === org.id)?.seats.find(s => !before.has(s.id))
            if (made) setSelected(made.id)
            setAdding(null)
          }} />
      )}
      {renaming && (
        <NameModal title="Rename the org" label="Name" id="org-name" action="Rename" onClose={() => setRenaming(false)}
          onSubmit={async name => { setOrgs(await core.renameOrg(org.id, name)); setRenaming(false) }} />
      )}
    </div>
  )
}

/** Where an agent sits: its chain of seats up to the top, and its reports,
 *  each an @mention that opens that report's profile. */
export function OrgPlace({ workspaceId, agent, onOpenProfile, onOpenOrg }: {
  workspaceId: string | null; agent: Agent; onOpenProfile: (h: Holder) => void; onOpenOrg: () => void
}) {
  const [orgs] = useOrgs()
  const [agents, setAgents] = useState<Agent[]>([])
  const [meId, setMeId] = useState<string | null>(null)
  const me = useMe()
  useEffect(() => { core.agents().then(setAgents, () => {}); core.meId().then(setMeId, () => {}) }, [])
  const org = orgs?.find(o => workspaceId && o.workspaces.includes(workspaceId)) ?? orgs?.[0]
  if (!org) return null
  const mine = org.seats.find(s => s.holder?.kind === 'agent' && s.holder.id === agent.id)
  if (!mine) return <p className="faint hint">{agent.name} doesn’t have a seat in {org.name} yet. <button type="button" className="link" onClick={onOpenOrg}>Give it one in Org</button></p>
  const chain: Seat[] = []
  for (let s: Seat | undefined = mine; s; s = org.seats.find(x => x.id === s?.parent)) chain.unshift(s)
  const reports = org.seats.filter(s => s.parent === mine.id)
  const who = { id: meId ?? '', name: me?.name ?? 'You' }
  return (
    <div className="orgplace">
      <div className="chain">
        {chain.map((s, i) => (
          <div key={s.id} className="chainstep">
            {i > 0 && <span className="vline" aria-hidden="true"><i /></span>}
            <div className={s.id === mine.id ? 'chainrow me' : 'chainrow'}>
              <SeatAvatar seat={s} agents={agents} />
              <span className="col"><span className="t">{s.title}</span><span className="n">{holderName(s.holder, agents, who) ?? 'Open'}</span></span>
            </div>
          </div>
        ))}
      </div>
      <p className="reports">
        {reports.length === 0 ? <span className="faint">No reports.</span> : <>
          <span className="faint">Reports:</span>{' '}
          {reports.map((r, i) => {
            const name = holderName(r.holder, agents, who)
            return (
              <span key={r.id}>
                {i > 0 && ', '}
                {r.holder && name
                  ? <button type="button" className="mention mentionlink" title={r.title} onClick={() => onOpenProfile(r.holder!)}>@{name}</button>
                  : <span className="faint">{r.title} (open)</span>}
              </span>
            )
          })}
        </>}
      </p>
    </div>
  )
}

/** A channel's access: open to the org, to teams in it, and to named
 *  members, and who that comes to right now. */
export function AccessPanel({ channelId, channelName, workspaceId, onClose }: { channelId: string; channelName: string; workspaceId: string; onClose: () => void }) {
  const [rows, setRows] = useState<AgentAccess[] | null>(null)
  const [rules, setRules] = useState<ChannelRules | null>(null)
  const [orgs] = useOrgs()
  const [error, setError] = useState('')
  useEffect(() => {
    core.channelAccess(channelId).then(setRows, e => setError(errorText(e)))
    core.channelRules(channelId).then(setRules, e => setError(errorText(e)))
  }, [channelId])
  const org = orgs?.find(o => o.workspaces.includes(workspaceId))
  const agents = rows?.map(r => r.agent) ?? []

  async function act(f: () => Promise<AgentAccess[]>) {
    setError('')
    try {
      setRows(await f())
      setRules(await core.channelRules(channelId))
      window.dispatchEvent(new Event('gild:reach'))
    } catch (e) { setError(errorText(e)) }
  }
  const setOrgRule = (on: boolean) => rules && act(() => core.setChannelRules(channelId, on, rules.teams))
  const setTeams = (teams: string[]) => rules && act(() => core.setChannelRules(channelId, rules.org, teams))

  const named = rows?.filter(r => r.via?.kind === 'named') ?? []
  const inHere = rows?.filter(r => r.access) ?? []
  const seat = (id: string) => org?.seats.find(s => s.id === id)
  const why = (r: AgentAccess) => r.via?.kind === 'named' ? 'named' : r.via?.kind === 'org' ? `everyone in ${org?.name ?? 'the org'}` : r.via?.kind === 'team' ? `${seat(r.via.seat)?.title ?? 'a removed seat'}’s team` : ''

  return (
    <aside className="panel" aria-label={`Access to #${channelName}`}>
      <div className="panelhead"><span className="label">Access to #{channelName}</span><button className="ghost" onClick={onClose}>Close</button></div>
      <div className="panelform access">
        <p className="soft" style={{ margin: 0, fontSize: 13 }}>People in the workspace can use every channel. An agent is here if any rule below lets it in, and every change is saved in the channel’s history.</p>
        {org && rules && (
          <>
            <section className="arule" aria-label="Org">
              <Toggle id="rule-org" label={`Everyone in ${org.name}`} hint="Every agent with a seat in the org, including ones added later." on={rules.org} onChange={setOrgRule} />
            </section>
            <section className="arule" aria-label="Teams">
              <span className="flabel">Teams <span className="faint">· a seat and everyone under it</span></span>
              <div className="achips">
                {rules.teams.map(t => {
                  const s = seat(t)
                  return (
                    <span key={t} className="achip">
                      {s ? <SeatAvatar seat={s} agents={agents} size={18} /> : null}
                      {s ? `${s.title}’s team` : 'A removed seat'}{s && <span className="n">{s.team}</span>}
                      <button className="x" aria-label={`Close to ${s?.title ?? 'that'} team`} onClick={() => setTeams(rules.teams.filter(x => x !== t))}>×</button>
                    </span>
                  )
                })}
                <select className="field add" aria-label="Add a team" value="" onChange={e => e.target.value && setTeams([...rules.teams, e.target.value])}>
                  <option value="">+ Add a team</option>
                  {org.seats.filter(s => !rules.teams.includes(s.id)).map(s => <option key={s.id} value={s.id}>{' '.repeat((s.level - 1) * 2)}{s.title} ({s.team})</option>)}
                </select>
              </div>
            </section>
          </>
        )}
        <section className="arule" aria-label="Members">
          <span className="flabel">Members <span className="faint">· named one by one</span></span>
          <div className="achips">
            {named.map(r => (
              <span key={r.agent.id} className="achip"><AgentAvatar agent={r.agent} />{r.agent.name}
                <button className="x" aria-label={`Remove ${r.agent.name}`} onClick={() => act(() => core.setAccess(channelId, r.agent.id, null))}>×</button>
              </span>
            ))}
            <select className="field add" aria-label="Add a member" value="" onChange={e => e.target.value && act(() => core.setAccess(channelId, e.target.value, 'write'))}>
              <option value="">+ Add a member</option>
              {rows?.filter(r => r.via?.kind !== 'named').map(r => <option key={r.agent.id} value={r.agent.id}>{r.agent.name}</option>)}
            </select>
          </div>
          {rows?.length === 0 && <p className="faint">No agents yet. Add one in the sidebar.</p>}
        </section>
        <section className="arule" aria-label="Who that is">
          <span className="flabel">Who that is <span className="faint">· {inHere.length} {inHere.length === 1 ? 'agent' : 'agents'}</span></span>
          {inHere.length === 0 && <p className="faint" style={{ margin: 0 }}>No agent can see this channel.</p>}
          {inHere.map(r => (
            <div key={r.agent.id} className="accessrow">
              <span className="aname"><AgentAvatar agent={r.agent} /><span className="col"><b>{r.agent.name}</b><span className="faint why">{why(r)}</span></span></span>
              <select id={`acc-${r.agent.id}`} aria-label={`${r.agent.name} can`} className="field" value={r.access ?? 'write'} onChange={e => act(() => core.setLevel(channelId, r.agent.id, e.target.value as Access))}>
                <option value="read">Can read</option>
                <option value="write">Can read and change</option>
              </select>
            </div>
          ))}
        </section>
        {error && <p className="error">{error}</p>}
      </div>
    </aside>
  )
}

type Kind = 'person' | 'agent' | 'open'

function SeatPanel({ org, seat, agents, meId, meName, channels, onOrgs, onRemoved }: {
  org: Org; seat: Seat; agents: Agent[]; meId: string | null; meName: string; channels: Channel[]
  onOrgs: (o: Org[]) => void; onRemoved: () => void
}) {
  const [title, setTitle] = useState(seat.title)
  const [kind, setKind] = useState<Kind>(seat.holder?.kind ?? 'open')
  const [agentId, setAgentId] = useState(seat.holder?.kind === 'agent' ? seat.holder.id : '')
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null)
  const [teamChannels, setTeamChannels] = useState<Channel[]>([])
  const [removing, setRemoving] = useState(false)
  useEffect(() => {
    Promise.all(channels.map(c => core.channelRules(c.id).then(r => (r.teams.includes(seat.id) ? c : null))))
      .then(cs => setTeamChannels(cs.filter((c): c is Channel => !!c)), () => {})
  }, [channels, seat.id])
  const seatOf = (id: string) => org.seats.find(s => s.holder?.kind === 'agent' && s.holder.id === id && s.id !== seat.id)
  const holder: Holder | null = kind === 'person' && meId ? { kind: 'person', id: meId } : kind === 'agent' && agentId ? { kind: 'agent', id: agentId } : null
  const dirty = title !== seat.title || JSON.stringify(holder) !== JSON.stringify(seat.holder)
  const parent = org.seats.find(s => s.id === seat.parent)
  // Where it can move: any seat outside its own team (in the outline, the
  // seat and the deeper seats right after it). The core refuses the rest.
  const at = org.seats.findIndex(s => s.id === seat.id)
  const team = new Set(org.seats.slice(at, at + seat.team).map(s => s.id))
  const meSeat = org.seats.find(s => s.holder?.kind === 'person' && s.id !== seat.id)

  async function act(f: () => Promise<void>) {
    setNote(null)
    try { await f() } catch (e) { setNote({ ok: false, text: errorText(e) }) }
  }

  return (
    <aside className="seatpanel" aria-label={`Seat: ${seat.title}`}>
      <span className="label">Seat</span>
      <div className="sfield">
        <label htmlFor="seat-edit-title" className="flabel">Title</label>
        <input id="seat-edit-title" className="field" value={title} maxLength={60} onChange={e => setTitle(e.target.value)} />
      </div>
      <div className="sfield">
        <span className="flabel" id="held-by">Held by</span>
        <div className="seg" role="radiogroup" aria-labelledby="held-by">
          {(['person', 'agent', 'open'] as Kind[]).map(k => (
            <button key={k} type="button" role="radio" aria-checked={kind === k} className={kind === k ? 'on' : ''} onClick={() => setKind(k)}>
              {k === 'person' ? 'A person' : k === 'agent' ? 'An agent' : 'Open'}
            </button>
          ))}
        </div>
        {kind === 'person' && (
          meSeat
            ? <p className="faint hint">You hold {meSeat.title}. Other people take seats when they join gild.</p>
            : <p className="faint hint">You ({meName}). Other people take seats when they join gild.</p>
        )}
        {kind === 'agent' && (
          <select id="seat-agent" className="field" aria-label="Agent" value={agentId} onChange={e => setAgentId(e.target.value)}>
            <option value="">Pick an agent</option>
            {agents.map(a => {
              const other = seatOf(a.id)
              return <option key={a.id} value={a.id} disabled={!!other}>{a.name}{other ? ` (holds ${other.title})` : ''}</option>
            })}
          </select>
        )}
        {kind === 'open' && <p className="faint hint">Open until someone takes it. Channels given to its team stay given.</p>}
      </div>
      {parent && (
        <div className="sfield">
          <label htmlFor="seat-parent" className="flabel">Reports to</label>
          <select id="seat-parent" className="field" value={parent.id} onChange={e => act(async () => { onOrgs(await core.moveSeat(seat.id, e.target.value)) })}>
            {org.seats.filter(s => !team.has(s.id)).map(s => <option key={s.id} value={s.id}>{'  '.repeat(s.level - 1)}{s.title}</option>)}
          </select>
          <span className="faint hint">Moving a seat moves its whole team, and the channels open to that team go with it.</span>
        </div>
      )}
      {teamChannels.length > 0 && (
        <div className="sfield">
          <span className="flabel">Channels open to this team</span>
          <div className="achips">{teamChannels.map(c => <span key={c.id} className="achip">#{c.name}</span>)}</div>
        </div>
      )}
      <div className="row">
        <button className="primary" disabled={!dirty || (kind === 'agent' && !agentId) || (kind === 'person' && !!meSeat)} onClick={() => act(async () => {
          onOrgs(await core.setSeat(seat.id, title, holder))
          setNote({ ok: true, text: 'Saved.' })
        })}>Save</button>
        {note && <span className={note.ok ? 'ok' : 'error'}>{note.text}</span>}
      </div>
      {parent && (
        <div className="row removebar">
          {removing ? (
            <>
              <p className="soft confirm">Remove {seat.title}?{seat.team > 1 ? ` Its reports will report to ${parent.title}.` : ''}</p>
              <button className="danger" onClick={() => act(async () => { onOrgs(await core.removeSeat(seat.id)); onRemoved() })}>Remove seat</button>
              <button className="ghost" onClick={() => setRemoving(false)}>Keep it</button>
            </>
          ) : <button className="ghost" onClick={() => setRemoving(true)}>Remove seat</button>}
        </div>
      )}
    </aside>
  )
}
