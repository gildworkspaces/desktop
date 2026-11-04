// The window's only way to the workspace: the Rust core's commands.
// Types mirror src-tauri/src/store.rs.
import { invoke, isTauri } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

export type Account = { name: string; demo: boolean }
export type Workspace = { id: string; name: string; channels: number; messages: number; created_at: number }
export type Channel = { id: string; workspace_id: string; name: string; topic: string; created_at: number }
export type Message = { id: string; channel_id: string; author: string; kind: 'person' | 'system' | 'agent'; body: string; created_at: number }

export type View = { kind: 'table'; fields: string[]; statuses: string[] }
export type DataRecord = { id: string; title: string; status: string; fields: Record<string, string>; created_at: number }
export type ChannelData = { view: View | null; records: DataRecord[] }
export type Change = { commit: string; author: string; at: number; message: string }

export type Access = 'read' | 'write'
export type Agent = { id: string; name: string; created_at: number }
export type NewAgent = { agent: Agent; token: string; endpoint: string }
export type AgentAccess = { agent: Agent; access: Access | null }
export type AuditEntry = { agent: string; action: string; detail: string; ok: boolean; at: number }

export const inApp = () => isTauri()

/** Call `f` with a channel id whenever an agent changes that channel. */
export function onChanged(f: (channelId: string) => void): () => void {
  let stop: (() => void) | null = null
  let gone = false
  listen<string>('gild://changed', e => f(e.payload)).then(u => { if (gone) u(); else stop = u })
  return () => { gone = true; stop?.() }
}

export const core = {
  account: () => invoke<Account>('account'),
  workspaces: () => invoke<Workspace[]>('workspaces'),
  createWorkspace: (name: string) => invoke<Workspace>('create_workspace', { name }),
  channels: (workspaceId: string) => invoke<Channel[]>('channels', { workspaceId }),
  createChannel: (workspaceId: string, name: string, topic = '') => invoke<Channel>('create_channel', { workspaceId, name, topic }),
  messages: (channelId: string) => invoke<Message[]>('messages', { channelId }),
  send: (channelId: string, body: string) => invoke<Message>('send_message', { channelId, body }),
  channelData: (channelId: string) => invoke<ChannelData>('channel_data', { channelId }),
  setView: (channelId: string, fields: string[], statuses: string[]) => invoke<ChannelData>('set_view', { channelId, fields, statuses }),
  addRecord: (channelId: string, title: string, status: string, fields: Record<string, string> = {}) =>
    invoke<DataRecord>('add_record', { channelId, title, status, fields }),
  updateRecord: (channelId: string, recordId: string, title: string, status: string, fields: Record<string, string>) =>
    invoke<DataRecord>('update_record', { channelId, recordId, title, status, fields }),
  recordHistory: (channelId: string, recordId: string) => invoke<Change[]>('record_history', { channelId, recordId }),
  agents: () => invoke<Agent[]>('agents'),
  createAgent: (name: string) => invoke<NewAgent>('create_agent', { name }),
  channelAccess: (channelId: string) => invoke<AgentAccess[]>('channel_access', { channelId }),
  setAccess: (channelId: string, agentId: string, access: Access | null) => invoke<AgentAccess[]>('set_access', { channelId, agentId, access }),
  activity: (channelId: string) => invoke<AuditEntry[]>('activity', { channelId }),
  mcpStatus: () => invoke<string>('mcp_status'),
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
