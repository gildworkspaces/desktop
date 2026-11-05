'use client'
// A message as a channel shows it, and a thread: the message it hangs off,
// its replies, and a composer for replying there.
import { useCallback, useEffect, useRef, useState } from 'react'
import { core, onChanged, mentionParts, initial, clock, day, errorText, type Message } from '@/lib/core'

function replyLine(m: Message): string {
  const n = m.replies ?? 0
  const last = m.last_reply_at ? ` · last ${day(m.last_reply_at) === 'Today' ? clock(m.last_reply_at) : `${day(m.last_reply_at)} ${clock(m.last_reply_at)}`}` : ''
  return `${n} ${n === 1 ? 'reply' : 'replies'}${last}`
}

export function MessageRow({ m, onThread }: { m: Message; onThread?: (m: Message) => void }) {
  return (
    <div className="msg">
      <div className={`avatar ${m.kind}`} aria-hidden="true">{initial(m.author)}</div>
      <div className="body">
        <div className="meta">
          <span className="author">{m.author}</span>
          {m.kind !== 'person' && <span className="badge">{m.kind}</span>}
          <span className="time">{clock(m.created_at)}</span>
        </div>
        <span className="text">{mentionParts(m.body).map((p, j) => (p.mention ? <span key={j} className="mention">{p.text}</span> : p.text))}</span>
        {onThread && (m.replies ?? 0) > 0 && (
          <button className="threadlink" onClick={() => onThread(m)}>{replyLine(m)}</button>
        )}
      </div>
      {onThread && (
        <button className="replybtn ghost" onClick={() => onThread(m)} aria-label={`Reply in thread to ${m.author}`}>Reply in thread</button>
      )}
    </div>
  )
}

export function ThreadPanel({ channelId, channelName, rootId, onReplied, onClose }: {
  channelId: string; channelName: string; rootId: string; onReplied: () => void; onClose: () => void
}) {
  const [thread, setThread] = useState<Message[] | null>(null)
  const [draft, setDraft] = useState('')
  const [error, setError] = useState('')
  const [version, setVersion] = useState(0)
  const list = useRef<HTMLDivElement>(null)

  useEffect(() => { core.thread(channelId, rootId).then(setThread, e => setError(errorText(e))) }, [channelId, rootId, version])
  // An agent replied: show it.
  useEffect(() => onChanged(id => { if (id === channelId) setVersion(v => v + 1) }), [channelId])
  const toBottom = useCallback(() => { const el = list.current; if (el) el.scrollTop = el.scrollHeight }, [])
  useEffect(toBottom, [thread, toBottom])

  async function send(e: React.FormEvent) {
    e.preventDefault()
    if (!draft.trim()) return
    setError('')
    try {
      const m = await core.reply(channelId, rootId, draft)
      setThread(t => (t ? [...t, m] : t))
      setDraft('')
      onReplied()
    } catch (err) { setError(errorText(err)) }
  }

  const [root, ...replies] = thread ?? []
  return (
    <aside className="panel threadpanel" aria-label={`Thread in #${channelName}`}>
      <div className="panelhead"><span className="label">Thread in #{channelName}</span><button className="ghost" onClick={onClose}>Close</button></div>
      <div className="list" ref={list}>
        {root && <MessageRow m={root} />}
        {root && <div className="threadrule"><span>{replies.length === 0 ? 'No replies yet' : `${replies.length} ${replies.length === 1 ? 'reply' : 'replies'}`}</span></div>}
        {replies.map(m => <MessageRow key={m.id} m={m} />)}
      </div>
      <div className="composer">
        <form onSubmit={send}>
          <label htmlFor="thread-composer" className="sr">Reply</label>
          <input id="thread-composer" autoFocus value={draft} onChange={e => setDraft(e.target.value)} placeholder="Reply in thread" />
          <button className="primary" disabled={!draft.trim()}>Reply</button>
        </form>
        <p className="faint" style={{ fontSize: 12, margin: '6px 2px 0' }}>Agents in this thread see your replies without an @mention, and answer when they have something to add.</p>
        {error && <p className="error">{error}</p>}
      </div>
    </aside>
  )
}
