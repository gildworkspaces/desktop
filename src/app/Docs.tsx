'use client'
// A channel's docs: markdown files in its history. People write them here;
// agents read and search them, and propose changes that wait for a person.
import { useEffect, useState } from 'react'
import { Markdown } from '@/lib/markdown'
import { core, clock, day, errorText, type ChannelDocs, type Change, type DiffLine, type Proposal } from '@/lib/core'

type Open = { kind: 'doc'; path: string } | { kind: 'proposal'; id: string } | null

export function DocsPane({ channelId, data, onChanged, onEditing }: { channelId: string; data: ChannelDocs; onChanged: () => void; onEditing: () => void }) {
  const [open, setOpen] = useState<Open>(null)
  const [title, setTitle] = useState('')
  const [error, setError] = useState('')

  // Something to show: what was open if it still exists, else the first
  // proposal waiting, else the first doc.
  const stillThere = open && (open.kind === 'doc' ? data.docs.some(d => d.path === open.path) : data.proposals.some(p => p.id === open.id))
  const current: Open = stillThere
    ? open
    : data.proposals[0] ? { kind: 'proposal', id: data.proposals[0].id } : data.docs[0] ? { kind: 'doc', path: data.docs[0].path } : null

  async function create(e: React.FormEvent) {
    e.preventDefault()
    setError('')
    try {
      const d = await core.createDoc(channelId, title)
      setTitle('')
      setOpen({ kind: 'doc', path: d.path })
      onChanged()
    } catch (err) { setError(errorText(err)) }
  }

  return (
    <div className="docs">
      <nav className="doclist" aria-label="Docs">
        {data.proposals.length > 0 && (
          <>
            <span className="label">Waiting for you</span>
            {data.proposals.map(p => (
              <button key={p.id} className={current?.kind === 'proposal' && current.id === p.id ? 'docitem on' : 'docitem'} onClick={() => setOpen({ kind: 'proposal', id: p.id })}>
                <span className="dot warn" aria-hidden="true" />
                <span>{p.agent} → {p.title}</span>
              </button>
            ))}
          </>
        )}
        <span className="label">Docs</span>
        {data.docs.map(d => (
          <button key={d.path} className={current?.kind === 'doc' && current.path === d.path ? 'docitem on' : 'docitem'} onClick={() => setOpen({ kind: 'doc', path: d.path })}>
            {d.title}
          </button>
        ))}
        <form onSubmit={create}>
          <label htmlFor="new-doc" className="sr">New doc</label>
          <input id="new-doc" className="field" placeholder="+ New doc" value={title} onChange={e => setTitle(e.target.value)} />
        </form>
        {error && <p className="error">{error}</p>}
      </nav>
      <div className="docmain">
        {!current && (
          <div className="empty">
            <span className="label">Docs</span>
            <p className="soft" style={{ margin: 0, maxWidth: '52ch' }}>
              Write this channel's how-tos, notes and plans as docs. Agents you share the channel with can search them to answer questions, and can propose changes that wait for your approval.
            </p>
          </div>
        )}
        {current?.kind === 'doc' && <DocView key={current.path} channelId={channelId} path={current.path} data={data} onChanged={onChanged} onEditing={onEditing} onReview={id => setOpen({ kind: 'proposal', id })} />}
        {current?.kind === 'proposal' && <ProposalView key={current.id} channelId={channelId} proposal={data.proposals.find(p => p.id === current.id)!} onChanged={onChanged} />}
      </div>
    </div>
  )
}

function DocView({ channelId, path, data, onChanged, onEditing, onReview }: { channelId: string; path: string; data: ChannelDocs; onChanged: () => void; onEditing: () => void; onReview: (id: string) => void }) {
  const doc = data.docs.find(d => d.path === path)!
  const [editing, setEditing] = useState(false)
  const [body, setBody] = useState(doc.body)
  const [history, setHistory] = useState<Change[] | null>(null)
  const [error, setError] = useState('')
  const proposals = data.proposals.filter(p => p.path === path)
  const waiting = proposals.length

  useEffect(() => { if (!editing) setBody(doc.body) }, [doc.body, editing])

  async function save() {
    setError('')
    try { await core.saveDoc(channelId, path, body); setEditing(false); setHistory(null); onChanged() } catch (err) { setError(errorText(err)) }
  }
  async function toggleHistory() {
    if (history) return setHistory(null)
    try { setHistory(await core.docHistory(channelId, path)) } catch (err) { setError(errorText(err)) }
  }

  return (
    <article className="doc" aria-label={`Doc: ${doc.title}`}>
      <div className="dochead">
        <span className="faint mono" style={{ fontSize: 12 }}>{path}</span>
        {waiting > 0 && <button className="chip waiting" onClick={() => onReview(proposals[0].id)}>Review {waiting} {waiting === 1 ? 'change' : 'changes'} waiting</button>}
        <span className="spacer" />
        {editing ? (
          <>
            <button className="primary" onClick={save}>Save</button>
            <button className="ghost" onClick={() => { setEditing(false); setBody(doc.body) }}>Cancel</button>
          </>
        ) : (
          <>
            <button className="ghost" onClick={toggleHistory}>{history ? 'Hide history' : 'History'}</button>
            <button className="ghost" onClick={() => { setEditing(true); onEditing() }}>Edit</button>
          </>
        )}
      </div>
      {error && <p className="error">{error}</p>}
      {history && (
        <div className="history inline">
          {history.map(h => (
            <div key={h.commit} className="change">
              <span>{h.message}</span>
              <span className="faint mono" style={{ fontSize: 11 }}>{h.author} · {day(h.at)} {clock(h.at)} · {h.commit.slice(0, 7)}</span>
            </div>
          ))}
        </div>
      )}
      {editing
        ? <><label htmlFor="doc-body" className="sr">Doc text, in markdown</label><textarea id="doc-body" className="field doctext" value={body} onChange={e => setBody(e.target.value)} spellCheck autoFocus /></>
        : <Markdown text={doc.body} />}
    </article>
  )
}

/** Long runs of unchanged lines fold to their first and last two. */
function fold(diff: DiffLine[]): (DiffLine | { fold: number })[] {
  const out: (DiffLine | { fold: number })[] = []
  let i = 0
  while (i < diff.length) {
    if (diff[i].op !== 'same') { out.push(diff[i++]); continue }
    let j = i
    while (j < diff.length && diff[j].op === 'same') j++
    const run = diff.slice(i, j)
    const head = i === 0 ? 0 : 2
    const tail = j === diff.length ? 0 : 2
    if (run.length > head + tail + 1) {
      out.push(...run.slice(0, head), { fold: run.length - head - tail }, ...run.slice(run.length - tail))
    } else out.push(...run)
    i = j
  }
  return out
}

function ProposalView({ channelId, proposal: p, onChanged }: { channelId: string; proposal: Proposal; onChanged: () => void }) {
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  async function decide(approve: boolean) {
    setBusy(true)
    setError('')
    try {
      await (approve ? core.approveProposal(channelId, p.id) : core.rejectProposal(channelId, p.id))
      onChanged()
    } catch (err) { setError(errorText(err)); setBusy(false) }
  }
  return (
    <article className="doc" aria-label={`Proposed change to ${p.title}`}>
      <div className="dochead">
        <span><b>{p.agent}</b> <span className="badge">agent</span> {p.new_doc ? 'proposes a new doc' : 'proposes a change to'} <b>{p.title}</b></span>
        <span className="spacer" />
        <button className="primary" onClick={() => decide(true)} disabled={busy || p.stale}>Approve</button>
        <button className="ghost" onClick={() => decide(false)} disabled={busy}>Reject</button>
      </div>
      <blockquote className="summary">{p.summary}<span className="faint mono" style={{ fontSize: 11 }}> · {day(p.at)} {clock(p.at)}</span></blockquote>
      {p.stale && <p className="error">The doc changed after this was proposed, so it can't be approved as it is. Reject it and ask {p.agent} for a fresh one.</p>}
      {error && <p className="error">{error}</p>}
      <div className="diff" role="table" aria-label="Changes">
        {fold(p.diff).map((l, i) => 'fold' in l
          ? <div key={i} className="dl fold" role="row">⋯ {l.fold} unchanged {l.fold === 1 ? 'line' : 'lines'}</div>
          : <div key={i} className={`dl ${l.op}`} role="row"><span className="sign" aria-label={l.op === 'add' ? 'added' : l.op === 'del' ? 'removed' : undefined}>{l.op === 'add' ? '+' : l.op === 'del' ? '−' : ' '}</span><span className="t">{l.text || ' '}</span></div>)}
      </div>
    </article>
  )
}
