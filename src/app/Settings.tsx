'use client'
// Settings: a two-column modal. Personal (you), App (how gild looks, its
// agents) and, inside a workspace, that workspace (export, import). New
// sections are one entry in SECTIONS and one page below.
import { useEffect, useRef, useState } from 'react'
import { revealItemInDir } from '@tauri-apps/plugin-opener'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { core, errorText, BANNERS, MAX_BIO, type Channel, type Profile, type Theme, type Workspace } from '@/lib/core'
import { Avatar, type SettingsSection } from './Profile'
import { AgentsSettings } from './Agents'

type Group = { label: string; items: { key: SettingsSection; label: string }[] }

export function SettingsModal({ section, profile, onProfile, workspace, channels, current, onImported, onClose }: {
  section: SettingsSection; profile: Profile; onProfile: (p: Profile) => void
  workspace: Workspace | null; channels: Channel[]; current: Channel | null
  onImported: (w: Workspace) => void; onClose: () => void
}) {
  const [at, setAt] = useState<SettingsSection>(section)
  const [version, setVersion] = useState('')
  useEffect(() => { core.version().then(setVersion, () => {}) }, [])
  useEffect(() => {
    const esc = (e: KeyboardEvent) => { if (e.key === 'Escape' && !document.querySelector('.modalback.top')) onClose() }
    document.addEventListener('keydown', esc)
    return () => document.removeEventListener('keydown', esc)
  }, [onClose])

  const groups: Group[] = [
    { label: 'Personal', items: [{ key: 'profile', label: 'My Profile' }] },
    { label: 'App', items: [{ key: 'appearance', label: 'Appearance' }, { key: 'agents', label: 'Agents' }] },
    { label: workspace ? workspace.name : 'Workspaces', items: [{ key: 'data', label: 'Export and import' }] },
  ]
  const title = groups.flatMap(g => g.items).find(i => i.key === at)?.label ?? ''

  return (
    <div className="modalback settingsback" role="dialog" aria-modal="true" aria-labelledby="settings-title">
      <div className="settings">
        <nav className="snav" aria-label="Settings sections">
          <div className="sme">
            <Avatar profile={profile} size={40} />
            <span className="col"><b>{profile.name}</b><button className="link" onClick={() => setAt('profile')}>Edit profile</button></span>
          </div>
          {groups.map(g => (
            <div key={g.label} className="sgroup">
              <span className="label">{g.label}</span>
              {g.items.map(i => (
                <button key={i.key} className={at === i.key ? 'sitem on' : 'sitem'} aria-current={at === i.key ? 'page' : undefined} onClick={() => setAt(i.key)}>{i.label}</button>
              ))}
            </div>
          ))}
          {version && <span className="sver mono">gild {version}</span>}
        </nav>
        <section className="smain">
          <header className="shead">
            <h1 id="settings-title">{title}</h1>
            <button className="iconbtn" onClick={onClose} aria-label="Close settings">
              <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18" /></svg>
            </button>
          </header>
          {at === 'profile' && <ProfilePage profile={profile} onProfile={onProfile} />}
          {at === 'appearance' && <AppearancePage profile={profile} onProfile={onProfile} />}
          {at === 'agents' && <AgentsSettings channels={channels} current={current} />}
          {at === 'data' && <DataPage workspace={workspace} onImported={onImported} />}
        </section>
      </div>
    </div>
  )
}

/** A picture the person picked, as a small square JPEG data: URL. */
async function smallPicture(file: File): Promise<string> {
  const url = URL.createObjectURL(file)
  try {
    const img = await new Promise<HTMLImageElement>((ok, bad) => {
      const i = new Image()
      i.onload = () => ok(i)
      i.onerror = () => bad(new Error('That file isn’t a picture gild can read.'))
      i.src = url
    })
    const side = 256
    const canvas = document.createElement('canvas')
    canvas.width = canvas.height = side
    const ctx = canvas.getContext('2d')
    if (!ctx) throw new Error('Couldn’t read that picture.')
    const s = Math.min(img.width, img.height)
    ctx.drawImage(img, (img.width - s) / 2, (img.height - s) / 2, s, s, 0, 0, side, side)
    return canvas.toDataURL('image/jpeg', 0.85)
  } finally {
    URL.revokeObjectURL(url)
  }
}

function ProfilePage({ profile, onProfile }: { profile: Profile; onProfile: (p: Profile) => void }) {
  const [name, setName] = useState(profile.name)
  const [bio, setBio] = useState(profile.bio)
  const [picture, setPicture] = useState<string | null>(profile.picture)
  const [banner, setBanner] = useState(profile.banner)
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null)
  const file = useRef<HTMLInputElement>(null)
  const dirty = name !== profile.name || bio !== profile.bio || picture !== profile.picture || banner !== profile.banner
  const draft = { ...profile, name: name.trim() || profile.name, bio, picture, banner }

  async function save() {
    setNote(null)
    try { onProfile(await core.saveProfile(name, bio, picture, banner)); setNote({ ok: true, text: 'Saved.' }) } catch (e) { setNote({ ok: false, text: errorText(e) }) }
  }
  async function choose(f: File | undefined) {
    if (!f) return
    try { setPicture(await smallPicture(f)); setNote(null) } catch (e) { setNote({ ok: false, text: errorText(e) }) }
  }

  return (
    <>
      <div className="spage two">
        <div className="sform">
          <div className="sfield">
            <label htmlFor="display-name" className="flabel">Display name</label>
            <input id="display-name" className="field" value={name} maxLength={40} onChange={e => setName(e.target.value)} />
            <span className="faint hint">Shown on your messages and the changes you make. Earlier messages keep the name they were sent with.</span>
          </div>
          <div className="sfield">
            <label htmlFor="bio" className="flabel">About me</label>
            <textarea id="bio" className="field" rows={3} maxLength={MAX_BIO} value={bio} onChange={e => setBio(e.target.value)} />
            <span className="faint hint mono" style={{ textAlign: 'right' }}>{bio.length} / {MAX_BIO}</span>
          </div>
          <div className="sfield">
            <span className="flabel">Picture</span>
            <div className="row">
              <button type="button" className="primary" onClick={() => file.current?.click()}>Choose a picture</button>
              {picture && <button type="button" className="ghost" onClick={() => setPicture(null)}>Remove</button>}
              <input ref={file} type="file" accept="image/*" hidden onChange={e => { choose(e.target.files?.[0]); e.target.value = '' }} />
            </div>
          </div>
          <div className="sfield">
            <span className="flabel" id="banner-label">Banner colour</span>
            <div className="row" role="radiogroup" aria-labelledby="banner-label">
              {BANNERS.map(c => (
                <button key={c} type="button" role="radio" aria-checked={banner === c} aria-label={c} className={banner === c ? 'swatch on' : 'swatch'} style={{ background: c }} onClick={() => setBanner(c)} />
              ))}
            </div>
          </div>
        </div>
        <div className="sfield">
          <span className="label">Preview</span>
          <div className="pcard">
            <div className="pbanner" style={{ background: banner }} />
            <div className="pbody">
              <div className="pbig"><Avatar profile={draft} size={76} status={profile.status} ring="var(--sunk)" /></div>
              <div className="pname">{draft.name}</div>
              {bio.trim() && <p className="pbio">{bio}</p>}
            </div>
          </div>
        </div>
      </div>
      <footer className="sfoot">
        {note && <span className={note.ok ? 'ok' : 'error'}>{note.text}</span>}
        <span className="spacer" />
        <button className="ghost" disabled={!dirty} onClick={() => { setName(profile.name); setBio(profile.bio); setPicture(profile.picture); setBanner(profile.banner); setNote(null) }}>Reset</button>
        <button className="primary" disabled={!dirty} onClick={save}>Save changes</button>
      </footer>
    </>
  )
}

const THEMES: { value: Theme; label: string }[] = [
  { value: 'system', label: 'System' },
  { value: 'light', label: 'Light' },
  { value: 'dark', label: 'Dark' },
]

function AppearancePage({ profile, onProfile }: { profile: Profile; onProfile: (p: Profile) => void }) {
  const [error, setError] = useState('')
  return (
    <div className="spage">
      <div className="sfield">
        <span className="flabel" id="theme-label">Theme</span>
        <div className="seg" role="radiogroup" aria-labelledby="theme-label">
          {THEMES.map(t => (
            <button key={t.value} role="radio" aria-checked={profile.theme === t.value} className={profile.theme === t.value ? 'on' : ''}
              onClick={async () => { setError(''); try { onProfile(await core.setTheme(t.value)) } catch (e) { setError(errorText(e)) } }}>{t.label}</button>
          ))}
        </div>
        <span className="faint hint">System follows your computer’s light or dark setting.</span>
      </div>
      {error && <p className="error">{error}</p>}
    </div>
  )
}

function DataPage({ workspace, onImported }: { workspace: Workspace | null; onImported: (w: Workspace) => void }) {
  const [exported, setExported] = useState<{ busy: boolean; path?: string; error?: string }>({ busy: false })
  const [imported, setImported] = useState<{ busy: boolean; error?: string }>({ busy: false })
  async function exportIt() {
    if (!workspace) return
    setExported({ busy: true })
    try { setExported({ busy: false, path: await core.exportWorkspace(workspace.id) }) } catch (e) { setExported({ busy: false, error: errorText(e) }) }
  }
  async function importIt() {
    const folder = await openDialog({ directory: true, title: 'Pick a folder Export made' })
    if (typeof folder !== 'string') return
    setImported({ busy: true })
    try { onImported(await core.importWorkspace(folder)) } catch (e) { setImported({ busy: false, error: errorText(e) }) }
  }
  return (
    <div className="spage">
      {workspace && (
        <div className="sline">
          <span className="col">
            <b>Export {workspace.name}</b>
            <span className="soft">A folder of open files in Documents/gild exports: chat as markdown, tables as CSV, docs as markdown, the full history as git, and a page that browses it all offline.</span>
            {exported.path && <span className="ok">Saved to <span className="mono">{exported.path.split('/').slice(-2).join('/')}</span></span>}
            {exported.error && <span className="error">{exported.error}</span>}
          </span>
          <span className="row">
            {exported.path && <button className="ghost" onClick={() => revealItemInDir(exported.path!)}>Show in Finder</button>}
            <button className="primary" onClick={exportIt} disabled={exported.busy}>{exported.busy ? 'Exporting…' : 'Export'}</button>
          </span>
        </div>
      )}
      <div className="sline">
        <span className="col">
          <b>Import a workspace</b>
          <span className="soft">Pick a folder that Export made. It becomes a new workspace with its channels, chat, threads, tables, docs and history.</span>
          {imported.error && <span className="error">{imported.error}</span>}
        </span>
        <button className="ghost" onClick={importIt} disabled={imported.busy}>{imported.busy ? 'Importing…' : 'Choose a folder'}</button>
      </div>
    </div>
  )
}
