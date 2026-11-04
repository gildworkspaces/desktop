'use client'
// The channel's view: the top of the channel, showing the data the
// conversation is about. Today that's a table, shown as a table or a board.
import { useEffect, useState } from 'react'
import { core, splitList, clock, day, errorText, type ChannelData, type DataRecord, type Change, type View } from '@/lib/core'

const PILLS = ['p0', 'p1', 'p2', 'p3']
const pill = (view: View, status: string) => `pill ${PILLS[Math.max(0, view.statuses.indexOf(status)) % PILLS.length]}`

export function ViewPane({ channelId, mode, onCount }: { channelId: string; mode: 'table' | 'board'; onCount: (n: number | null) => void }) {
  const [data, setData] = useState<ChannelData | null>(null)
  const [editingColumns, setEditingColumns] = useState(false)
  const [open, setOpen] = useState<DataRecord | null>(null)
  const [error, setError] = useState('')

  const load = () => core.channelData(channelId).then(d => { setData(d); onCount(d.view ? d.records.length : null) }, e => setError(errorText(e)))
  useEffect(() => { load() }, [channelId]) // eslint-disable-line react-hooks/exhaustive-deps

  if (error) return <div className="empty"><p className="error">{error}</p></div>
  if (!data) return null
  if (!data.view || editingColumns) {
    return <ColumnsForm channelId={channelId} view={data.view} onDone={d => { setData(d); setEditingColumns(false); onCount(d.records.length) }} onCancel={data.view ? () => setEditingColumns(false) : undefined} />
  }
  const view = data.view
  return (
    <div className="data">
      <AddRecord channelId={channelId} view={view} onAdded={load} onColumns={() => setEditingColumns(true)} />
      {data.records.length === 0 && <p className="faint" style={{ padding: '6px 16px' }}>No records yet. Add the first one above.</p>}
      {data.records.length > 0 && (mode === 'table'
        ? <Table view={view} records={data.records} onOpen={setOpen} />
        : <Board channelId={channelId} view={view} records={data.records} onOpen={setOpen} onMoved={load} />)}
      {open && <RecordPanel channelId={channelId} view={view} record={open} onClose={() => setOpen(null)} onSaved={r => { setOpen(r); load() }} />}
    </div>
  )
}

function ColumnsForm({ channelId, view, onDone, onCancel }: { channelId: string; view: View | null; onDone: (d: ChannelData) => void; onCancel?: () => void }) {
  const [fields, setFields] = useState(view ? view.fields.join(', ') : '')
  const [statuses, setStatuses] = useState(view ? view.statuses.join(', ') : 'To do, Doing, Done')
  const [error, setError] = useState('')
  async function save(e: React.FormEvent) {
    e.preventDefault()
    setError('')
    try { onDone(await core.setView(channelId, splitList(fields), splitList(statuses))) } catch (err) { setError(errorText(err)) }
  }
  return (
    <form className="empty columns" onSubmit={save}>
      <span className="label">{view ? 'Change the table' : "This channel's view"}</span>
      {!view && <p className="soft" style={{ margin: 0, maxWidth: '50ch' }}>Give this channel a table for the things it's about: students, tasks, orders. Each row keeps its full history.</p>}
      <div className="colgrid">
        <label htmlFor="cols">Columns <span className="faint">after the title, comma-separated</span></label>
        <input id="cols" className="field" placeholder="Sent by, Program, Start" value={fields} onChange={e => setFields(e.target.value)} />
        <label htmlFor="stats">Statuses <span className="faint">the board's columns, in order</span></label>
        <input id="stats" className="field" value={statuses} onChange={e => setStatuses(e.target.value)} />
      </div>
      <div style={{ display: 'flex', gap: 8 }}>
        <button className="primary">{view ? 'Save table' : 'Add a table'}</button>
        {onCancel && <button type="button" className="ghost" onClick={onCancel}>Cancel</button>}
      </div>
      {error && <p className="error">{error}</p>}
    </form>
  )
}

function AddRecord({ channelId, view, onAdded, onColumns }: { channelId: string; view: View; onAdded: () => void; onColumns: () => void }) {
  const [title, setTitle] = useState('')
  const [status, setStatus] = useState(view.statuses[0])
  const [error, setError] = useState('')
  async function add(e: React.FormEvent) {
    e.preventDefault()
    setError('')
    try { await core.addRecord(channelId, title, status); setTitle(''); onAdded() } catch (err) { setError(errorText(err)) }
  }
  return (
    <form className="addrec" onSubmit={add}>
      <label htmlFor="rec-title" className="sr">New record</label>
      <input id="rec-title" className="field" placeholder="+ Add a record" value={title} onChange={e => setTitle(e.target.value)} />
      <label htmlFor="rec-status" className="sr">Status</label>
      <select id="rec-status" className="field" value={status} onChange={e => setStatus(e.target.value)}>
        {view.statuses.map(s => <option key={s}>{s}</option>)}
      </select>
      <button className="ghost" disabled={!title.trim()}>Add</button>
      <span className="spacer" />
      <button type="button" className="link" onClick={onColumns}>Columns</button>
      {error && <p className="error" style={{ flexBasis: '100%' }}>{error}</p>}
    </form>
  )
}

function Table({ view, records, onOpen }: { view: View; records: DataRecord[]; onOpen: (r: DataRecord) => void }) {
  return (
    <div className="tablewrap">
      <table>
        <thead><tr><th>Title</th>{view.fields.map(f => <th key={f}>{f}</th>)}<th>Status</th></tr></thead>
        <tbody>
          {records.map(r => (
            <tr key={r.id} onClick={() => onOpen(r)}>
              <td><button className="rowlink" onClick={() => onOpen(r)}>{r.title}</button></td>
              {view.fields.map(f => <td key={f} className="soft">{r.fields[f] ?? ''}</td>)}
              <td><span className={pill(view, r.status)}>{r.status}</span></td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

function Board({ channelId, view, records, onOpen, onMoved }: { channelId: string; view: View; records: DataRecord[]; onOpen: (r: DataRecord) => void; onMoved: () => void }) {
  const [error, setError] = useState('')
  async function move(r: DataRecord, status: string) {
    setError('')
    try { await core.updateRecord(channelId, r.id, r.title, status, r.fields); onMoved() } catch (err) { setError(errorText(err)) }
  }
  return (
    <div className="board" style={{ gridTemplateColumns: `repeat(${view.statuses.length}, minmax(170px, 1fr))` }}>
      {error && <p className="error" style={{ gridColumn: '1 / -1' }}>{error}</p>}
      {view.statuses.map(s => {
        const cards = records.filter(r => r.status === s)
        return (
          <div key={s} className="col">
            <div className="colhead"><span className={pill(view, s)}>{s}</span><span className="faint">{cards.length}</span></div>
            {cards.map(r => (
              <div key={r.id} className="bcard">
                <button className="rowlink" onClick={() => onOpen(r)}>{r.title}</button>
                {view.fields.slice(0, 2).some(f => r.fields[f]) && <span className="faint">{view.fields.slice(0, 2).map(f => r.fields[f]).filter(Boolean).join(' · ')}</span>}
                <label className="sr" htmlFor={`mv-${r.id}`}>Move {r.title}</label>
                <select id={`mv-${r.id}`} className="move" value={r.status} onChange={e => move(r, e.target.value)}>
                  {view.statuses.map(o => <option key={o}>{o}</option>)}
                </select>
              </div>
            ))}
          </div>
        )
      })}
    </div>
  )
}

function RecordPanel({ channelId, view, record, onClose, onSaved }: { channelId: string; view: View; record: DataRecord; onClose: () => void; onSaved: (r: DataRecord) => void }) {
  const [title, setTitle] = useState(record.title)
  const [status, setStatus] = useState(record.status)
  const [fields, setFields] = useState<Record<string, string>>(record.fields)
  const [history, setHistory] = useState<Change[]>([])
  const [error, setError] = useState('')
  useEffect(() => {
    setTitle(record.title); setStatus(record.status); setFields(record.fields)
    core.recordHistory(channelId, record.id).then(setHistory, e => setError(errorText(e)))
  }, [channelId, record])
  async function save(e: React.FormEvent) {
    e.preventDefault()
    setError('')
    try { onSaved(await core.updateRecord(channelId, record.id, title, status, fields)) } catch (err) { setError(errorText(err)) }
  }
  return (
    <aside className="panel" aria-label={`Record: ${record.title}`}>
      <div className="panelhead">
        <span className="label">Record</span>
        <button className="ghost" onClick={onClose} aria-label="Close">Close</button>
      </div>
      <form onSubmit={save} className="panelform">
        <label htmlFor="r-title">Title</label>
        <input id="r-title" className="field" value={title} onChange={e => setTitle(e.target.value)} />
        <label htmlFor="r-status">Status</label>
        <select id="r-status" className="field" value={status} onChange={e => setStatus(e.target.value)}>
          {view.statuses.map(s => <option key={s}>{s}</option>)}
        </select>
        {view.fields.map((f, i) => (
          <div key={f} style={{ display: 'contents' }}>
            <label htmlFor={`r-f${i}`}>{f}</label>
            <input id={`r-f${i}`} className="field" value={fields[f] ?? ''} onChange={e => setFields(x => ({ ...x, [f]: e.target.value }))} />
          </div>
        ))}
        <button className="primary" style={{ marginTop: 6 }}>Save</button>
        {error && <p className="error">{error}</p>}
      </form>
      <div className="history">
        <span className="label">History</span>
        {history.map(h => (
          <div key={h.commit} className="change">
            <span>{h.message}</span>
            <span className="faint mono" style={{ fontSize: 11 }}>{h.author} · {day(h.at)} {clock(h.at)} · {h.commit.slice(0, 7)}</span>
          </div>
        ))}
      </div>
    </aside>
  )
}
