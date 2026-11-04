# gild desktop

gild is the open workspace where people and agents work together: channels
for conversation, a view on each channel for the data it's about, and agents
as members with permissions.

This is the desktop app. It works on its own computer today; the plan for
everything after that is the design doc.

## What works (0.4.0)

- A workspaces home: create workspaces, open them.
- Inside a workspace: channels, with the top third of each channel kept for
  its view and the bottom two thirds for messages. **Expand view** flips the
  split.
- Messages, channels and workspaces are saved in a SQLite database on this
  computer and are there when you reopen the app.
- **A channel can hold a table**, shown as a table or a board. Add records,
  edit them, move them between statuses, and open a record to see its full
  history: who changed what, and when.
- **Agents.** Add an agent in the sidebar and gild shows its token once,
  with the command that connects Claude Code (any MCP client works). Agents
  see only the channels you share with them, from a channel's **Access**
  button: *can read*, or *can read and change*. What agents do appears in
  the channel as it happens, and every call they make, allowed or refused, is
  in the channel's **Activity**.
- A built-in **demo account**. Accounts and sign-in come later; until then
  everything is written as "Demo".

The first run creates a "Demo workspace" with #general.

## Where things are

- `src/app/page.tsx`: the interface (vinext, exported as static files that
  the Tauri window serves from disk).
- `src/lib/core.ts`: the window's only way to data, the Rust core's commands.
- `src-tauri/src/store.rs`: the database, written with Turso's Rust engine
  (`turso_core`, the engine Zega+SQLite uses). No C.
- `src-tauri/src/records.rs`: each channel's records live in a git
  repository, `channels/<channel id>.git` in the data folder, as
  `.gild/view.json` and one `.gild/records/<id>.json` per record. Git is the
  truth: every change is a commit by the person who made it, and the SQLite
  tables are an index rebuilt from it (on every write, and on startup when a
  channel's history has moved past its index). Pure Rust (gitoxide).
- `src-tauri/src/mcp.rs`: the agents' way in, an MCP server (Streamable
  HTTP, rmcp) at `http://127.0.0.1:4747/mcp`, on this computer only. Tools:
  `list_channels`, `read_channel`, `post_message`, `add_record`,
  `update_record`, `record_history`. Each call carries the agent's token;
  access is checked by the same store functions the window uses, and a
  channel's grants live in its git history as `.gild/acl.json`.
- The database file: `gild.db` in the app's data folder
  (`~/Library/Application Support/gg.gild.desktop/` on macOS). It is a plain
  SQLite file; `sqlite3` reads it.

## Develop

```sh
npm ci
npx tauri dev        # the app, with the interface on vite dev
npm run check        # TypeScript
npm run build        # the interface's static export (dist/client)
cd src-tauri && cargo test --lib   # the store
```

The Linux CI also runs the built app through tauri-driver
(`tests/e2e/smoke.mjs`): it opens the demo workspace, makes a channel, sends a
message and reads it back through the core.

## License

Apache-2.0.
