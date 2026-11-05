'use client'
// Settings: a two-column modal. Personal (you), App (how gild looks, its
// agents) and, inside a workspace, that workspace (export, import). New
// sections are one entry in SECTIONS and one page below.
import { useEffect, useRef, useState } from 'react'
import { revealItemInDir } from '@tauri-apps/plugin-opener'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { core, errorText, BANNERS, MAX_BIO, type Agent, type Channel, type Grant, type Manifest, type Profile, type ProviderInfo, type Theme, type Workspace } from '@/lib/core'
import { Avatar, type SettingsSection } from './Profile'
import { AgentPage, AgentsSettings, Toggle } from './Agents'

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
    { label: 'App', items: [{ key: 'appearance', label: 'Appearance' }, { key: 'agents', label: 'Agents' }, { key: 'providers', label: 'Providers' }, { key: 'addons', label: 'Add-ons' }] },
    { label: workspace ? workspace.name : 'Workspaces', items: [{ key: 'data', label: 'Export and import' }] },
  ]
  // An agent's or add-on's own page sits under its section.
  const parent: SettingsSection = at.startsWith('agent:') ? 'agents' : at.startsWith('addon:') ? 'addons' : at
  const title = groups.flatMap(g => g.items).find(i => i.key === parent)?.label ?? ''

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
                <button key={i.key} className={parent === i.key ? 'sitem on' : 'sitem'} aria-current={parent === i.key ? 'page' : undefined} onClick={() => setAt(i.key)}>{i.label}</button>
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
          {at === 'agents' && <AgentsSettings channels={channels} current={current} onOpenAgent={a => setAt(`agent:${a.id}`)} />}
          {at.startsWith('agent:') && <AgentPage key={at} agentId={at.slice(6)} channels={channels} current={current} onBack={() => setAt('agents')} />}
          {at === 'providers' && <ProvidersPage />}
          {at === 'addons' && <AddonsPage onPermissions={id => setAt(`addon:${id}`)} />}
          {at.startsWith('addon:') && <AddonPermissions key={at} addonId={at.slice(6)} onBack={() => setAt('addons')} />}
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

/** Settings › Providers: where cloud agents' models run. */
function ProvidersPage() {
  const [list, setList] = useState<ProviderInfo[] | null>(null)
  const [account, setAccount] = useState('')
  const [token, setToken] = useState('')
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null)
  const [busy, setBusy] = useState(false)
  useEffect(() => { core.providers().then(l => { setList(l); setAccount(l[0]?.account_id ?? '') }, e => setNote({ ok: false, text: errorText(e) })) }, [])
  const p = list?.[0]
  if (!p) return note ? <p className="error" style={{ padding: 32 }}>{note.text}</p> : null
  async function save() {
    setBusy(true); setNote(null)
    try { setList(await core.saveProvider(p!.id, account, token || null)); setToken(''); setNote({ ok: true, text: `Saved. The token is in ${p!.vault === 'harar' ? 'harar' : 'your keychain'}.` }) } catch (e) { setNote({ ok: false, text: errorText(e) }) } finally { setBusy(false) }
  }
  async function check() {
    setBusy(true); setNote(null)
    try { const ms = await core.providerModels(p!.id); setNote({ ok: true, text: `Connected: ${ms.length} models, ${ms.filter(m => m.tools).length} of them can use tools.` }) } catch (e) { setNote({ ok: false, text: errorText(e) }) } finally { setBusy(false) }
  }
  return (
    <div className="spage">
      <p className="soft lead">An agent can run on this computer (Claude Code, Codex) or on a provider’s model in the cloud. For a cloud agent, gild calls the model and runs the tools it asks for here; the provider never connects to this computer.</p>
      <div className="addon">
        <div className="row"><b>{p.name}</b>{p.account_id && p.has_token && <span className="tag">set up</span>}</div>
        <div className="sfield">
          <label htmlFor="cf-account" className="flabel">Account ID</label>
          <input id="cf-account" className="field mono" value={account} onChange={e => setAccount(e.target.value)} spellCheck={false} placeholder="From the Cloudflare dashboard" />
        </div>
        <div className="sfield">
          <label htmlFor="cf-token" className="flabel">API token</label>
          <input id="cf-token" className="field mono" type="password" value={token} onChange={e => setToken(e.target.value)} spellCheck={false} autoComplete="off"
            placeholder={p.has_token ? `Saved in ${p.vault === 'harar' ? 'harar' : 'your keychain'}. Paste a new one to replace it.` : 'A token with Workers AI access'} disabled={p.vault === 'none'} />
          <span className="faint hint">
            {p.vault === 'harar' && 'Kept in harar, your vault, not in gild’s database, so agents on this computer can’t read it.'}
            {p.vault === 'keychain' && 'Kept in your system keychain, not in gild’s database. Install harar (harar.addis.gg) and gild keeps it there instead.'}
            {p.vault === 'none' && 'gild keeps provider tokens in harar, your vault. Install it first: curl -fsSL https://harar.addis.gg/install.sh | sh'}
          </span>
        </div>
        <div className="row">
          <button className="primary" onClick={save} disabled={busy}>Save</button>
          <button className="ghost" onClick={check} disabled={busy || !p.account_id || !p.has_token}>Check</button>
          {p.has_token && <button className="ghost" onClick={async () => { setBusy(true); try { setList(await core.saveProvider(p.id, account, '')); setNote({ ok: true, text: `The token was removed from ${p.vault === 'harar' ? 'harar' : 'your keychain'}.` }) } catch (e) { setNote({ ok: false, text: errorText(e) }) } finally { setBusy(false) } }} disabled={busy}>Remove token</button>}
        </div>
        {note && <span className={note.ok ? 'ok' : 'error'}>{note.text}</span>}
      </div>
    </div>
  )
}

/** Settings › Add-ons: what each add-on adds and what it may touch. */
function AddonsPage({ onPermissions }: { onPermissions: (id: string) => void }) {
  const [list, setList] = useState<Manifest[] | null>(null)
  const [error, setError] = useState('')
  useEffect(() => { core.addons().then(setList, e => setError(errorText(e))) }, [])
  const none = (xs: string[]) => (xs.length === 0 ? 'none' : xs.join(', '))
  return (
    <div className="spage">
      <p className="soft lead">Add-ons give gild new kinds of files. Each file type’s operations are what you and agents edit it with: every one is an agent tool, a commit and an Activity entry.</p>
      {error && <p className="error">{error}</p>}
      {list?.map(m => (
        <div key={m.id} className="addon">
          <div className="row"><b>{m.name}</b><span className="tag">{m.version}</span><span className="faint" style={{ fontSize: 12 }}>by {m.publisher}</span><span className="spacer" /><button className="ghost" onClick={() => onPermissions(m.id)}>Permissions</button></div>
          <p className="soft" style={{ margin: 0, fontSize: 13 }}>{m.description}</p>
          <p className="faint" style={{ margin: 0, fontSize: 12 }}>Runs as a WebAssembly module in a sandbox: it reaches no files, network or clock except what gild hands it, with limits on memory and time.</p>
          {m.contributes.fileTypes.map(t => (
            <div key={t.id} className="addonpart">
              <span className="flabel">{t.name} files ({t.extensions.join(', ')})</span>
              <ul>{t.operations.map(o => <li key={o.name}><span className="mono">{t.id}_{o.name}</span> <span className="faint">{o.description}</span></li>)}</ul>
            </div>
          ))}
          <div className="addonpart">
            <span className="flabel">May touch</span>
            <span className="faint" style={{ fontSize: 12 }}>Files: {none(m.permissions.files)} · Network: {none(m.permissions.network)} · Secrets: {none(m.permissions.secrets)}</span>
          </div>
        </div>
      ))}
    </div>
  )
}

const KIND: Record<string, string> = { files: 'Edit files matching', network: 'Reach', secrets: 'Use the secret' }

/** Settings › Add-ons › an add-on › Permissions: what it may touch, and which agents may use its tools. */
function AddonPermissions({ addonId, onBack }: { addonId: string; onBack: () => void }) {
  const [manifest, setManifest] = useState<Manifest | null>(null)
  const [grants, setGrants] = useState<Grant[]>([])
  const [agents, setAgents] = useState<Agent[]>([])
  const [off, setOff] = useState<Record<string, string[]>>({})
  const [error, setError] = useState('')
  useEffect(() => {
    core.addons().then(ms => setManifest(ms.find(m => m.id === addonId) ?? null), e => setError(errorText(e)))
    core.addonGrants(addonId).then(setGrants, e => setError(errorText(e)))
    core.agents().then(async list => {
      setAgents(list)
      const entries = await Promise.all(list.map(a => core.agentToolsOff(a.id).then(o => [a.id, o] as const)))
      setOff(Object.fromEntries(entries))
    }, e => setError(errorText(e)))
  }, [addonId])
  if (!manifest) return <div className="spage"><button className="link back" onClick={onBack}>← Add-ons</button>{error && <p className="error">{error}</p>}</div>
  const tools = manifest.contributes.fileTypes.flatMap(t => t.operations.map(o => `${t.id}_${o.name}`))
  async function grant(g: Grant, v: boolean) {
    setError('')
    try { setGrants(await core.setAddonGrant(addonId, g.kind, g.value, v)) } catch (e) { setError(errorText(e)) }
  }
  async function allow(agentId: string, names: string[], v: boolean) {
    setError('')
    try { let last: string[] = off[agentId] ?? []; for (const n of names) last = await core.setAgentTool(agentId, n, v); setOff(o => ({ ...o, [agentId]: last })) } catch (e) { setError(errorText(e)) }
  }
  return (
    <div className="spage">
      <button className="link back" onClick={onBack}>← Add-ons</button>
      <h2 style={{ fontSize: 20 }}>{manifest.name} permissions</h2>
      <section className="toolgroup" aria-label={`What ${manifest.name} may touch`}>
        <span className="flabel">What {manifest.name} may touch</span>
        {grants.length === 0 && <p className="faint">It asks for nothing.</p>}
        {grants.map(g => (
          <Toggle key={g.kind + g.value} id={`grant-${g.kind}-${g.value}`} label={`${KIND[g.kind]} ${g.value}`}
            hint={g.kind === 'files' ? `Off: ${manifest.name} edits nothing that matches, for everyone.` : undefined} on={g.granted} onChange={v => grant(g, v)} />
        ))}
        {(['network', 'secrets'] as const).filter(k => !grants.some(g => g.kind === k)).map(k => <p key={k} className="faint hint">{k === 'network' ? 'It reaches no network.' : 'It uses no secrets.'}</p>)}
      </section>
      <section className="toolgroup" aria-label="Which agents can use its tools">
        <span className="flabel">Which agents can use its tools</span>
        {agents.length === 0 && <p className="faint">No agents yet.</p>}
        {agents.length > 0 && (
          <div className="matrix" role="table" aria-label={`${manifest.name} tools by agent`} style={{ '--cols': tools.length + 1 } as React.CSSProperties}>
            <div className="mrow head" role="row"><span role="columnheader">Agent</span>{tools.map(t => <span key={t} role="columnheader" className="mono">{t.replace(/^[^_]+_/, '').replace(/_/g, '_\u200b')}</span>)}<span role="columnheader">All</span></div>
            {agents.map(a => {
              const mine = off[a.id] ?? []
              const allOn = tools.every(t => !mine.includes(t))
              return (
                <div key={a.id} className="mrow" role="row">
                  <span role="rowheader"><b>{a.name}</b></span>
                  {tools.map(t => (
                    <span key={t} role="cell"><button role="switch" aria-checked={!mine.includes(t)} aria-label={`${a.name}: ${t}`} className={mine.includes(t) ? 'switch' : 'switch on'} onClick={() => allow(a.id, [t], mine.includes(t))}><span /></button></span>
                  ))}
                  <span role="cell"><button className="link" onClick={() => allow(a.id, tools, !allOn)}>{allOn ? 'All off' : 'All on'}</button></span>
                </div>
              )
            })}
          </div>
        )}
        <span className="faint hint">The same switches are on each agent’s Tools tab.</span>
      </section>
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
