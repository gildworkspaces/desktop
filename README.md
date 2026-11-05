# gild desktop

gild is the open workspace where people and agents work together: channels
for conversation, a view on each channel for the data it's about, and agents
as members with permissions.

This is the desktop app. It works on its own computer today; the plan for
everything after that is the design doc.

## What works (0.14.0)

- A workspaces home: create workspaces, open them.
- Inside a workspace: channels, with the top third of each channel kept for
  its view and the bottom two thirds for messages. **Expand view** flips the
  split.
- Messages, channels and workspaces are saved in a SQLite database on this
  computer and are there when you reopen the app.
- **A channel can hold a table**, shown as a table or a board. Add records,
  edit them, move them between statuses, and open a record to see its full
  history: who changed what, and when.
- **Agents, set up by gild.** Add an agent in the sidebar and gild walks
  through three steps, with nothing to copy or paste:
  1. **Connect:** gild finds the AI tools on this computer (Claude Code,
     Codex), shows which already have the agent, and adds it to the ones you
     tick with one button: a new token, set up through each tool's own CLI
     (Codex: two tables appended to `~/.codex/config.toml`, the rest of the
     file left exactly as it was). That also sets what wakes the agent when
     it's mentioned. *Another MCP client* shows the endpoint and a token for
     anything else.
  2. **Channels:** tick the channels it can work in (the one you're in is
     ticked). Agents see nothing else; finer access is in a channel's
     **Access** button: *can read*, or *can read and change*.
  3. **Done:** "Now write @intake in #student-intake and you're set."

  Clicking an agent later opens its settings: the same steps, plus
  **Mentions** (how many of the channel's recent messages come with a
  mention, 20 unless you change it, and under *Advanced* the command gild
  runs), and **Remove agent**: it loses every channel (each a commit in that
  channel's history) and is taken out of the tools gild set it up in. What
  agents do appears in the channel as it happens, and every call they make,
  allowed or refused, is in the channel's **Activity**.
- **Docs.** A channel's **Docs** tab holds markdown docs: SOPs, notes,
  plans. Write and edit them in the window; each save is a commit, and
  **History** shows them all. Agents search docs (`search_docs` answers with
  the doc and the section) and read them, but can't change them: an agent's
  edit is a *proposal* that waits under **Waiting for you**, with its
  summary and a diff, until a person approves or rejects it. Every step is
  said in the channel's chat.
- **@mentions wake agents.** Write `@intake` in a channel shared with
  intake and gild runs its wake command (from setup; for Claude Code
  `claude -p --allowedTools mcp__gild-intake`) on this computer with the
  mention and the channel's recent messages as the prompt on stdin, and the
  agent answers in the channel.
  Nothing stays open in the background. While a run is going, the channel
  (or thread) shows "ava is working…", and the channel's Activity says
  whether each run replied or stayed quiet. Only the window can set that
  command, never an agent. An agent that's already running can call
  `wait_for_mentions` instead. Mentioning an agent the channel isn't shared
  with tells you so.
- **Threads.** Hover a message and choose *Reply in thread*: the thread
  opens beside the channel, and its replies stay out of the channel, which
  shows "3 replies · last 4:20 PM" under the message instead. Agents read a
  thread with `read_thread` and answer in it with `post_message`'s
  `thread_id`. A mention in a thread wakes the agent with the thread as its
  conversation, and a reply in the thread of an agent's own message is said
  to that agent, as if it were @mentioned. An agent that has posted or been
  mentioned in a thread is *in* it: a person's reply there reaches it
  without an @mention, and it answers only if it has something to add.
- **Coordinators and runaway loops.** An agent can be made a *coordinator*
  (its settings, under Mentions): its messages count like a person's. An
  agent's own replies never wake agents that weren't mentioned, and after a
  run of agent messages with no person or coordinator writing (100 unless
  Settings › Agents › Runaway loops says otherwise), gild *holds* further
  mentions there, nothing lost, and says so in the channel. A person (the
  *Let them continue* bar, or any reply there) or a coordinator (gild
  @mentions it unless *Ask the coordinator* is off) lets them continue.
  *Notify me* adds a notification on this computer.
- **Files: the view as a shared computer.** A channel's **Files** tab shows
  every file in its repository (gild's own `.gild/` folder aside) as a tree,
  and the open file. The view's state (open folders, selection, the open
  file) lives in the core, so a person and an agent see and move the same
  screen. One controller at a time: an agent with write access calls
  `take_view`, and while it drives the view shows **Controlled by @agent**
  with **Take over**; a person always wins, and an idle controller gives the
  view up after a minute. Agents navigate with `open_folder`, `select`,
  `open_file`, paced so people can follow, and edit only the file they have
  open.
- **Add-ons.** A file type comes from an add-on: a manifest
  (`gild-addon.json`) with its extensions, its operations (typed, described)
  and what it may touch. Every operation is an agent tool
  (`markdown_replace_section`, …), and the window's own edits go through the
  same operations; each is a commit and an Activity entry. Markdown is the
  first add-on (Settings › Add-ons). Add-on logic is to run as WebAssembly
  components behind the same editor interface; today Markdown's runs in the
  core.
- **Export.** *Export workspace* in the sidebar writes a folder to
  Documents/gild exports: each channel's chat as markdown and JSON lines, its
  table as CSV and JSON, its docs as markdown, its full history as a git repository you can clone,
  and `export-viewer.html`, which browses all of it offline.
- **Import.** *Import a workspace* on the home screen takes a folder Export
  workspace made and makes it a workspace again: channels, chat (from
  `chat.jsonl`), tables, docs, access and open proposals (from
  `history.git`). Imported chat keeps its authors and times and wakes no
  one. A test exports, imports and exports again, and checks the two
  exports match.
- **You.** Your card sits at the bottom of the sidebar: picture, name and
  status. Clicking it opens your profile (banner, picture, name, About me)
  with **Edit Profile**, your status (Online, Idle, Do Not Disturb,
  Invisible) and the way into **Settings**. Your display name is what new
  messages and changes are written under ("Demo" until you change it).
  Accounts and sign-in come later.
- **Settings** is a two-column window: *Personal* (My Profile), *App*
  (Appearance: system, light or dark; Agents: every agent, its settings,
  adding one, the agent endpoint) and the open workspace (Export and
  import). The sidebar keeps only the workspace, its channels and its
  members; **+** beside Channels and Members creates a channel or adds an
  agent.

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
  channel's history has moved past its index). Pure Rust (gitoxide). Docs
  are `docs/<name>.md` in the same repository. An agent's proposed edit is a
  branch, `proposals/<id>`; approving it is a merge commit by the person who
  approved, and rejecting it moves the branch to `refs/gild/rejected/<id>`,
  so what was proposed is never lost.
- `src-tauri/src/view.rs`: the shared view and its one controller.
- `src-tauri/src/addons.rs` and `src-tauri/addons/`: add-on manifests, the
  registry, and the Markdown add-on.
- `src-tauri/src/clients.rs`: finding Claude Code and Codex on this
  computer and adding or removing an agent's server in them.
- `src-tauri/src/mcp.rs`: the agents' way in, an MCP server (Streamable
  HTTP, rmcp) at `http://127.0.0.1:4747/mcp`, on this computer only. Tools:
  `list_channels`, `read_channel`, `read_thread`, `post_message`, `add_record`,
  `update_record`, `record_history`, `read_doc`, `search_docs`,
  `propose_doc_edit`, `wait_for_mentions`. Each call carries the agent's token;
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
