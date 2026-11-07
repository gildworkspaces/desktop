'use client'
// You, as the app shows you: your picture, your card at the bottom of the
// sidebar, and the menu it opens (profile, status, settings).
import { createContext, useContext, useEffect, useRef, useState } from 'react'
import { core, errorText, initial, type Profile, type Status } from '@/lib/core'

/** You, for anything deep in the window that needs you (your messages' picture). */
export const MeContext = createContext<Profile | null>(null)
export const useMe = () => useContext(MeContext)

export const STATUS: { value: Status; label: string; hint?: string }[] = [
  { value: 'online', label: 'Online' },
  { value: 'idle', label: 'Idle', hint: 'Away from the keyboard' },
  { value: 'dnd', label: 'Do Not Disturb', hint: 'No notifications; agents still work' },
  { value: 'invisible', label: 'Invisible', hint: 'Others see you as offline' },
]

export const statusLabel = (s: Status) => STATUS.find(x => x.value === s)?.label ?? 'Online'

export function StatusDot({ status, size = 10, ring }: { status: Status; size?: number; ring?: string }) {
  return <span className={`sdot ${status}`} style={{ width: size, height: size, ...(ring ? { boxShadow: `0 0 0 3px ${ring}` } : {}) }} aria-hidden="true" />
}

export function Avatar({ profile, size, status, ring }: { profile: Pick<Profile, 'name' | 'picture' | 'banner'>; size: number; status?: Status; ring?: string }) {
  return (
    <span className="pavatar" style={{ width: size, height: size }}>
      {profile.picture
        ? <img src={profile.picture} alt="" width={size} height={size} />
        : <span className="pinitial" style={{ background: profile.banner, fontSize: Math.round(size * 0.4) }}>{initial(profile.name)}</span>}
      {status && <span className="pstatus"><StatusDot status={status} size={Math.max(10, Math.round(size * 0.24))} ring={ring ?? 'var(--panel)'} /></span>}
    </span>
  )
}

export type SettingsSection = 'profile' | 'appearance' | 'agents' | 'providers' | 'addons' | 'org' | 'data' | `agent:${string}` | `agentprofile:${string}` | `addon:${string}`

/** Your card: in the sidebar's foot, or (`compact`) in the home screen's top bar. */
export function UserCard({ profile, onProfile, onSettings, placement = 'up' }: {
  profile: Profile; onProfile: (p: Profile) => void; onSettings: (s: SettingsSection) => void; placement?: 'up' | 'down'
}) {
  const [open, setOpen] = useState(false)
  const box = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!open) return
    const away = (e: MouseEvent) => { if (box.current && !box.current.contains(e.target as Node)) setOpen(false) }
    const esc = (e: KeyboardEvent) => { if (e.key === 'Escape') setOpen(false) }
    document.addEventListener('mousedown', away)
    document.addEventListener('keydown', esc)
    return () => { document.removeEventListener('mousedown', away); document.removeEventListener('keydown', esc) }
  }, [open])
  const go = (s: SettingsSection) => { setOpen(false); onSettings(s) }
  return (
    <div className={`usercard ${placement}`} ref={box}>
      <button className="me" onClick={() => setOpen(o => !o)} aria-expanded={open} aria-haspopup="dialog" aria-label={`${profile.name}: profile and status`}>
        <Avatar profile={profile} size={34} status={profile.status} ring="var(--chip)" />
        <span className="who2">
          <span className="nm">{profile.name}</span>
          <span className="st">{statusLabel(profile.status)}</span>
        </span>
      </button>
      {placement === 'up' && (
        <button className="iconbtn" onClick={() => onSettings('appearance')} aria-label="App settings" title="App settings">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" /></svg>
        </button>
      )}
      {open && <ProfileMenu profile={profile} onProfile={onProfile} go={go} />}
    </div>
  )
}

function ProfileMenu({ profile, onProfile, go }: { profile: Profile; onProfile: (p: Profile) => void; go: (s: SettingsSection) => void }) {
  const [statusOpen, setStatusOpen] = useState(false)
  const [error, setError] = useState('')
  async function pick(s: Status) {
    setError('')
    try { onProfile(await core.setStatus(s)); setStatusOpen(false) } catch (e) { setError(errorText(e)) }
  }
  return (
    <div className="pmenu" role="dialog" aria-label="Your profile">
      <div className="pbanner" style={{ background: profile.banner }} />
      <div className="pbody">
        <div className="pbig"><Avatar profile={profile} size={76} status={profile.status} ring="var(--surface)" /></div>
        <div className="pname">{profile.name}</div>
        {profile.bio && <p className="pbio">{profile.bio}</p>}
        <div className="pitems">
          <button onClick={() => go('profile')}>
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M12 20h9" /><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z" /></svg>
            Edit Profile
          </button>
          <hr />
          <button onClick={() => setStatusOpen(o => !o)} aria-expanded={statusOpen} aria-haspopup="menu">
            <span className="iconslot"><StatusDot status={profile.status} /></span>
            {statusLabel(profile.status)}
            <svg className="chev" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M9 6l6 6-6 6" /></svg>
          </button>
          <hr />
          <button onClick={() => go('profile')}>
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><circle cx="12" cy="8" r="4" /><path d="M4 21c0-4 4-7 8-7s8 3 8 7" /></svg>
            Personal Settings
          </button>
          <button onClick={() => go('appearance')}>
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><rect x="3" y="4" width="18" height="14" rx="2" /><path d="M8 21h8" /><path d="M12 18v3" /></svg>
            App Settings
          </button>
        </div>
        {error && <p className="error">{error}</p>}
      </div>
      {statusOpen && (
        <div className="smenu" role="menu" aria-label="Status">
          {STATUS.map(s => (
            <button key={s.value} role="menuitemradio" aria-checked={profile.status === s.value} className={profile.status === s.value ? 'on' : ''} onClick={() => pick(s.value)}>
              <span className="iconslot"><StatusDot status={s.value} /></span>
              <span className="stext"><b>{s.label}</b>{s.hint && <span>{s.hint}</span>}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  )
}
