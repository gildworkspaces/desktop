// The window's only way to the workspace: the Rust core's commands.
// Types mirror src-tauri/src/store.rs.
import { invoke, isTauri } from '@tauri-apps/api/core'

export type Account = { name: string; demo: boolean }
export type Workspace = { id: string; name: string; channels: number; messages: number; created_at: number }
export type Channel = { id: string; workspace_id: string; name: string; topic: string; created_at: number }
export type Message = { id: string; channel_id: string; author: string; kind: 'person' | 'system' | 'agent'; body: string; created_at: number }

export const inApp = () => isTauri()

export const core = {
  account: () => invoke<Account>('account'),
  workspaces: () => invoke<Workspace[]>('workspaces'),
  createWorkspace: (name: string) => invoke<Workspace>('create_workspace', { name }),
  channels: (workspaceId: string) => invoke<Channel[]>('channels', { workspaceId }),
  createChannel: (workspaceId: string, name: string, topic = '') => invoke<Channel>('create_channel', { workspaceId, name, topic }),
  messages: (channelId: string) => invoke<Message[]>('messages', { channelId }),
  send: (channelId: string, body: string) => invoke<Message>('send_message', { channelId, body }),
}

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
