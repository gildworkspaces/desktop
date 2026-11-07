// The window's only way to the workspace: the Rust core's commands.
// Types mirror src-tauri/src/store.rs.
import { invoke, isTauri } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

export type Account = { name: string; demo: boolean }
/** The channel's view: one shared screen, one controller. */
export type FileEntry = { path: string; size: number }
export type Controller = { kind: 'none' } | { kind: 'person'; name: string; at: number } | { kind: 'agent'; name: string; at: number }
export type ViewState = { expanded: string[]; selected: string | null; open: string | null; controller: Controller }
export type ChannelView = { state: ViewState; files: FileEntry[]; text: string | null; file_type: string | null; operations: string[] }
export type Nav = { op: 'expand' | 'collapse' | 'select' | 'open'; path: string } | { op: 'close' }
/** An add-on's manifest (gild-addon.json). */
export type AddonOperation = { name: string; description: string; params: unknown }
export type Manifest = {
  id: string; name: string; version: string; publisher: string; description: string
  contributes: { fileTypes: { id: string; name: string; extensions: string[]; operations: AddonOperation[] }[] }
  permissions: { network: string[]; secrets: string[]; files: string[] }
}

/** An agent tool, as Settings lists it. */
export type ToolInfo = { name: string; description: string; group: string; addon: string | null }
export type Grant = { kind: 'files' | 'network' | 'secrets'; value: string; granted: boolean }

/** An agent running right now, and where it was called (thread: a thread in that channel). */
export type Working = { agent: string; channel_id: string; thread: string | null }
export type Status = 'online' | 'idle' | 'dnd' | 'invisible'
export type Theme = 'system' | 'light' | 'dark'
/** You: `picture` is a small data: URL; `banner` one of BANNERS. */
export type Profile = { name: string; bio: string; picture: string | null; banner: string; status: Status; theme: Theme }
export const BANNERS = ['#f38020', '#3b6fd8', '#2f8f62', '#a2509a', '#c2410c', '#4a4d55']
export const MAX_BIO = 190
export type Workspace = { id: string; name: string; channels: number; messages: number; created_at: number }
export type Channel = { id: string; workspace_id: string; name: string; topic: string; created_at: number }
/** `thread`: a reply, in the thread of that message. `replies` and
 *  `last_reply_at`: a message with a thread. */
export type Message = {
  id: string; channel_id: string; author: string; kind: 'person' | 'system' | 'agent'; body: string; created_at: number
  thread?: string; replies?: number; last_reply_at?: number
}

export type View = { kind: 'table'; fields: string[]; statuses: string[] }
export type DataRecord = { id: string; title: string; status: string; fields: Record<string, string>; created_at: number }
export type ChannelData = { view: View | null; records: DataRecord[] }
export type Change = { commit: string; author: string; at: number; message: string }

export type Doc = { path: string; title: string; body: string }
export type DiffLine = { op: 'same' | 'add' | 'del'; text: string }
export type Proposal = { id: string; agent: string; summary: string; at: number; path: string; title: string; new_doc: boolean; stale: boolean; diff: DiffLine[] }
export type ChannelDocs = { docs: Doc[]; proposals: Proposal[] }

export type Access = 'read' | 'write'
/** `wake`: the command gild runs when the agent is mentioned, if any. */
/** `context`: how many recent messages come with a mention. */
export type Runner = { provider: string; model: string }
/** `everyone`: may call @everyone. `picture`, `prompt`: its profile (the prompt comes with every wake). */
export type Agent = { id: string; name: string; created_at: number; wake?: string | null; context?: number; coordinator?: boolean; everyone?: boolean; runner?: Runner | null; picture?: string | null; prompt?: string }
/** The longest prompt an agent's profile takes (the core's MAX_AGENT_PROMPT). */
export const MAX_AGENT_PROMPT = 4000
/** A cloud provider (never its token). */
export type ProviderInfo = { id: string; name: string; account_id: string | null; has_token: boolean; vault: 'harar' | 'keychain' | 'none'; vault_error: string | null }
export type ModelInfo = { id: string; description: string; tools: boolean; context_window: number | null }
/** Settings › Agents › Runaway loops. */
export type LoopRules = { limit: number; ask_coordinator: boolean; notify_me: boolean }
export type NewAgent = { agent: Agent; token: string; endpoint: string }
/** Why an agent can be in a channel: named there, or through the org or a team (`seat`: the team's top seat). */
export type Via = { kind: 'named' } | { kind: 'org' } | { kind: 'team'; seat: string }
export type AgentAccess = { agent: Agent; access: Access | null; via: Via | null }
/** Who holds a seat: you (a person), an agent, or nobody yet (null). */
export type Holder = { kind: 'person' | 'agent'; id: string }
/** `team`: how many seats its team has, itself included. */
export type Seat = { id: string; title: string; parent: string | null; holder: Holder | null; level: number; team: number }
/** `seats` in outline order: each seat, then its reports. */
export type Org = { id: string; name: string; workspaces: string[]; seats: Seat[]; max_levels: number }
export type ChannelRules = { org: boolean; teams: string[] }
/** An MCP client on this computer that gild can set an agent up in. */
export type ClientStatus = { id: string; name: string; installed: boolean; connected: boolean; wake: string }
export type Connected = { token: string; endpoint: string; clients: ClientStatus[]; errors: string[]; agents: Agent[] }
export type AuditEntry = { agent: string; action: string; detail: string; ok: boolean; at: number }

export const inApp = () => isTauri()

/** Call `f` with a channel id whenever its view changes from the window. */
export function onView(f: (channelId: string) => void): () => void {
  let stop: (() => void) | null = null
  let gone = false
  listen<string>('gild://view', e => f(e.payload)).then(u => { if (gone) u(); else stop = u })
  return () => { gone = true; stop?.() }
}

/** Call `f` whenever an agent starts or finishes a run. */
export function onWorking(f: () => void): () => void {
  let stop: (() => void) | null = null
  let gone = false
  listen('gild://working', () => f()).then(u => { if (gone) u(); else stop = u })
  return () => { gone = true; stop?.() }
}

/** Call `f` with a channel id whenever an agent changes that channel. */
export function onChanged(f: (channelId: string) => void): () => void {
  let stop: (() => void) | null = null
  let gone = false
  listen<string>('gild://changed', e => f(e.payload)).then(u => { if (gone) u(); else stop = u })
  return () => { gone = true; stop?.() }
}

export const core = {
  account: () => invoke<Account>('account'),
  profile: () => invoke<Profile>('profile'),
  saveProfile: (name: string, bio: string, picture: string | null, banner: string) => invoke<Profile>('save_profile', { name, bio, picture, banner }),
  setStatus: (status: Status) => invoke<Profile>('set_status', { status }),
  setTheme: (theme: Theme) => invoke<Profile>('set_theme', { theme }),
  version: () => invoke<string>('version'),
  workspaces: () => invoke<Workspace[]>('workspaces'),
  createWorkspace: (name: string) => invoke<Workspace>('create_workspace', { name }),
  channels: (workspaceId: string) => invoke<Channel[]>('channels', { workspaceId }),
  createChannel: (workspaceId: string, name: string, topic = '') => invoke<Channel>('create_channel', { workspaceId, name, topic }),
  messages: (channelId: string) => invoke<Message[]>('messages', { channelId }),
  send: (channelId: string, body: string) => invoke<Message>('send_message', { channelId, body }),
  working: () => invoke<Working[]>('working'),
  addons: () => invoke<Manifest[]>('addons'),
  addToChannel: (channelId: string, agentId: string, messageId: string | null) => invoke<null>('add_to_channel', { channelId, agentId, messageId }),
  providers: () => invoke<ProviderInfo[]>('providers'),
  saveProvider: (provider: string, accountId: string, token: string | null) => invoke<ProviderInfo[]>('save_provider', { provider, accountId, token }),
  providerModels: (provider: string) => invoke<ModelInfo[]>('provider_models', { provider }),
  setRunner: (agentId: string, runner: Runner | null) => invoke<Agent[]>('set_runner', { agentId, runner }),
  toolCatalog: () => invoke<ToolInfo[]>('tool_catalog'),
  agentToolsOff: (agentId: string) => invoke<string[]>('agent_tools_off', { agentId }),
  setAgentTool: (agentId: string, tool: string, allowed: boolean) => invoke<string[]>('set_agent_tool', { agentId, tool, allowed }),
  addonGrants: (addonId: string) => invoke<Grant[]>('addon_grants', { addonId }),
  setAddonGrant: (addonId: string, kind: string, value: string, granted: boolean) => invoke<Grant[]>('set_addon_grant', { addonId, kind, value, granted }),
  channelView: (channelId: string) => invoke<ChannelView>('channel_view', { channelId }),
  driveView: (channelId: string, nav: Nav) => invoke<ChannelView>('drive_view', { channelId, nav }),
  editFile: (channelId: string, path: string, op: string, args: Record<string, unknown>) => invoke<ChannelView>('edit_file', { channelId, path, op, args }),
  takeOverView: (channelId: string) => invoke<ChannelView>('take_over_view', { channelId }),
  thread: (channelId: string, threadId: string) => invoke<Message[]>('thread', { channelId, threadId }),
  reply: (channelId: string, threadId: string, body: string) => invoke<Message>('send_reply', { channelId, threadId, body }),
  channelData: (channelId: string) => invoke<ChannelData>('channel_data', { channelId }),
  setView: (channelId: string, fields: string[], statuses: string[]) => invoke<ChannelData>('set_view', { channelId, fields, statuses }),
  addRecord: (channelId: string, title: string, status: string, fields: Record<string, string> = {}) =>
    invoke<DataRecord>('add_record', { channelId, title, status, fields }),
  updateRecord: (channelId: string, recordId: string, title: string, status: string, fields: Record<string, string>) =>
    invoke<DataRecord>('update_record', { channelId, recordId, title, status, fields }),
  recordHistory: (channelId: string, recordId: string) => invoke<Change[]>('record_history', { channelId, recordId }),
  channelDocs: (channelId: string) => invoke<ChannelDocs>('channel_docs', { channelId }),
  createDoc: (channelId: string, title: string) => invoke<Doc>('create_doc', { channelId, title }),
  saveDoc: (channelId: string, path: string, body: string) => invoke<Doc>('save_doc', { channelId, path, body }),
  docHistory: (channelId: string, path: string) => invoke<Change[]>('doc_history', { channelId, path }),
  approveProposal: (channelId: string, id: string) => invoke<ChannelDocs>('approve_proposal', { channelId, id }),
  rejectProposal: (channelId: string, id: string) => invoke<ChannelDocs>('reject_proposal', { channelId, id }),
  agents: () => invoke<Agent[]>('agents'),
  createAgent: (name: string) => invoke<NewAgent>('create_agent', { name }),
  agentClients: (agentId: string) => invoke<ClientStatus[]>('agent_clients', { agentId }),
  connectAgent: (agentId: string, clients: string[]) => invoke<Connected>('connect_agent', { agentId, clients }),
  removeAgent: (agentId: string) => invoke<Agent[]>('remove_agent', { agentId }),
  setContext: (agentId: string, messages: number) => invoke<Agent[]>('set_context', { agentId, messages }),
  setCoordinator: (agentId: string, on: boolean) => invoke<Agent[]>('set_coordinator', { agentId, on }),
  setEveryone: (agentId: string, on: boolean) => invoke<Agent[]>('set_everyone', { agentId, on }),
  saveAgentProfile: (agentId: string, picture: string | null, prompt: string) => invoke<Agent[]>('save_agent_profile', { agentId, picture, prompt }),
  loopRules: () => invoke<LoopRules>('loop_rules'),
  setLoopRules: (rules: LoopRules) => invoke<LoopRules>('set_loop_rules', { rules }),
  held: (channelId: string, threadId?: string) => invoke<number>('held', { channelId, threadId: threadId ?? null }),
  letAgentsContinue: (channelId: string, threadId?: string) => invoke<Message>('let_agents_continue', { channelId, threadId: threadId ?? null }),
  setWake: (agentId: string, command: string) => invoke<Agent[]>('set_wake', { agentId, command }),
  channelAccess: (channelId: string) => invoke<AgentAccess[]>('channel_access', { channelId }),
  setAccess: (channelId: string, agentId: string, access: Access | null) => invoke<AgentAccess[]>('set_access', { channelId, agentId, access }),
  setLevel: (channelId: string, agentId: string, access: Access) => invoke<AgentAccess[]>('set_level', { channelId, agentId, access }),
  channelRules: (channelId: string) => invoke<ChannelRules>('channel_rules', { channelId }),
  setChannelRules: (channelId: string, org: boolean, teams: string[]) => invoke<AgentAccess[]>('set_channel_rules', { channelId, org, teams }),
  orgs: () => invoke<Org[]>('orgs'),
  renameOrg: (orgId: string, name: string) => invoke<Org[]>('rename_org', { orgId, name }),
  addSeat: (parentId: string, title: string) => invoke<Org[]>('add_seat', { parentId, title }),
  setSeat: (seatId: string, title: string, holder: Holder | null) => invoke<Org[]>('set_seat', { seatId, title, holder }),
  moveSeat: (seatId: string, parentId: string) => invoke<Org[]>('move_seat', { seatId, parentId }),
  removeSeat: (seatId: string) => invoke<Org[]>('remove_seat', { seatId }),
  meId: () => invoke<string>('me_id'),
  activity: (channelId: string) => invoke<AuditEntry[]>('activity', { channelId }),
  mcpStatus: () => invoke<string>('mcp_status'),
  exportWorkspace: (workspaceId: string) => invoke<string>('export_workspace', { workspaceId }),
  importWorkspace: (folder: string) => invoke<Workspace>('import_workspace', { folder }),
}

/** A message's text split into plain parts and @mentions, the way the core
 *  reads them (an @ inside a word, as in an email address, isn't one). */
export function mentionParts(text: string): { text: string; mention: boolean }[] {
  const out: { text: string; mention: boolean }[] = []
  let last = 0
  for (const m of text.matchAll(/(^|[^\p{L}\p{N}.])@([\p{L}\p{N}_-]+)/gu)) {
    const at = (m.index ?? 0) + m[1].length
    if (at > last) out.push({ text: text.slice(last, at), mention: false })
    out.push({ text: '@' + m[2], mention: true })
    last = at + 1 + m[2].length
  }
  if (last < text.length) out.push({ text: text.slice(last), mention: false })
  return out
}

/** "Sent by, Program" → ["Sent by", "Program"] */
export const splitList = (s: string) => s.split(',').map(x => x.trim()).filter(Boolean)

// A workspace's colour, from its name: the same name is always the same colour.
const TINTS = ['#f38020', '#3b6fd8', '#2f8f62', '#a2509a', '#c2410c', '#4a4d55', '#8a5f08']
export function tint(name: string): string {
  let h = 0
  for (const c of name) h = (h * 31 + c.codePointAt(0)!) >>> 0
  return TINTS[h % TINTS.length]
}

export const initial = (name: string) => (name.trim()[0] ?? '?').toUpperCase()

export function clock(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
}

export function day(ms: number): string {
  const d = new Date(ms)
  const today = new Date()
  const yesterday = new Date(today.getTime() - 86_400_000)
  if (d.toDateString() === today.toDateString()) return 'Today'
  if (d.toDateString() === yesterday.toDateString()) return 'Yesterday'
  return d.toLocaleDateString([], { weekday: 'long', month: 'short', day: 'numeric' })
}

export const errorText = (e: unknown) => (typeof e === 'string' ? e : e instanceof Error ? e.message : String(e))
