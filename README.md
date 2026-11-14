# gild desktop

gild is the open workspace where people and agents work together: channels
for conversation, a view on each channel for the data it's about, and agents
as members with permissions.

This is the desktop app. It works on its own computer today; the plan for
everything after that is the design doc.

## What works (0.24.0)

- A workspaces home: create workspaces, open them.
- **A workspace is a folder you can open**: `~/Gild/<name>/` unless you pick
  another place when you create it ("Front Desk 2" if the name is taken).
  The folder is a git repository holding the workspace and #general;
  every other channel is its own repository in `channels/<name>/`, which
  the main one ignores. `gild.json` at the top lists the workspace (id,
  name, org) and its channels (id, name, topic, folder, remote). Click the
  workspace's name in the sidebar for **Show in Finder** (Explorer on
  Windows, the file manager on Linux). Every change gild makes is one
  commit by its author, and the files on disk match it. Edit or add files
  there with any other app and gild notices, commits them as you ("Edit
  outside gild: …") and updates the channel; a broken `.gild/*.json` is
  left uncommitted and the channel says why. Live chat stays in gild's
  database: it never goes in git. Data from 0.23 and earlier (a hidden
  repository per channel in the app's data folder) moves into these
  folders on the first launch, every commit, author, date and branch kept;
  each moved repository is checked object by object, and the old ones are
  kept (not deleted) in the data folder's `migrated-0.24/`. A channel that
  can't be moved keeps its old history and says why; gild still starts.
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
     Codex, Pi), shows which already have the agent, and adds it to the ones you
     tick with one button: a new token, set up through each tool's own CLI
     (Codex: two tables appended to `~/.codex/config.toml`, the rest of the
     file left exactly as it was; Pi: `pi mcp add` with the agent's tools
     declared directly, read back from `~/.pi/agent/mcp.json`). That also
     sets what wakes the agent when it's mentioned. **Pi** is how an agent
     runs on a model of your own, such as a local one through llama.cpp:
     small models tend to write their answer instead of calling
     `post_message`, so for a Pi agent gild posts what it writes as its
     reply (unless it posted itself, or wrote `NOTHING`). *Another MCP client* shows the endpoint and a token for
     anything else.
  2. **Channels:** a switch per channel it can work in, saved as you flip
     it (the one you're in starts on). Agents see nothing else; finer
     access, and the org and team rules, are in a channel's **Access** button: *can read*, or *can read
     and change*.
  3. **Done:** "Now write @intake in #student-intake and you're set."

  Clicking an agent later opens its settings: the same steps, plus
  **Mentions** (how many of the channel's recent messages come with a
  mention, 20 unless you change it, and under *Advanced* the command gild
  runs, plus the *Coordinator* and *Can call @everyone* switches), and
  **Remove agent**. Beside **Settings** in Settings › Agents, **Profile**
  sets the agent's picture (shown on its messages) and its instructions,
  which come with every wake, before the conversation. Remove agent: it
  loses every channel (each a commit in that
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
  `wait_for_mentions` instead. `@everyone` calls every agent that can see
  the channel (people can use it, and agents whose *Can call @everyone* is
  on; any other agent's `@everyone` wakes no one). A mention of an agent that can't see the channel is struck
  through, and hovering it offers *Add to channel*, which passes that
  message on; the composer warns before you send one.
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
  first add-on (Settings › Add-ons). An add-on's logic is a WebAssembly
  module the app runs in a sandbox: it may import nothing (no files,
  network or clock), and each call has 64 MB of memory and an instruction
  budget. gild's own add-ons are built the same way: each is a crate under
  `src-tauri/addons/<name>` that `build.rs` compiles to
  `wasm32-unknown-unknown` (`rustup target add wasm32-unknown-unknown`).
- **Cloud agents.** An agent runs on this computer (Claude Code, Codex) or
  in the cloud: its Connect tab, *Cloud*, then a provider and a model (the
  provider's live list, those that can use tools first). The first provider
  is Cloudflare Workers AI (Settings › Providers: account ID and an API
  token, kept in harar, Addis's vault, when it's installed (gild starts
  harar's agent if it isn't running), else the system
  keychain on macOS and Windows). When a cloud agent is mentioned, gild
  calls the model (OpenAI-compatible chat completions, streamed), and its
  reply grows in the channel as it's written; tool calls run through the
  same path as the MCP endpoint (same switches, Activity, limits). A run
  ends when the model answers, posts where it was asked, or repeats itself,
  and after 12 rounds at most. The provider never connects to this
  computer.
- **Permissions.** Every agent tool, gild's own and each add-on
  operation, can be turned on or off per agent: Settings › Agents › an
  agent › **Tools**. A tool that's off isn't offered to that agent, and a
  call to it is refused and logged. Settings › Add-ons › **Permissions**
  grants what an add-on asks to touch (Markdown: files matching `*.md`;
  off, it edits nothing) and shows which agents may use its tools. An
  agent's settings are a page in Settings, opened from the sidebar too.
- **Orgs.** Every workspace belongs to an org: Settings › *workspace* ›
  **Org** shows it as an outline of seats. A seat has a title, reports to
  one other seat, and is held by you, an agent, or nobody yet (open). The
  top seat leads it (you, as CEO, to start), and it goes at most five
  levels deep. A seat and everyone under it is a *team*; moving a seat
  moves its team. A channel's **Access** opens it to any of: everyone in
  the org (new channels start that way), whole teams, and named members,
  then lists who that comes to and why, with *can read* or *can read and
  change* for each. An agent without a seat gets in only by name. Each
  change is a commit in the channel's history. An agent's **Profile**
  shows where it sits, up to the top, and its reports as @mentions that
  open their profiles. Removing an agent leaves its seat open. Seats are
  the groundwork for more people: someone who joins takes a seat, and the
  team and channels under it are already theirs.
- **Export.** *Export workspace* in the sidebar writes a folder to
  Documents/gild exports: each channel's chat as markdown and JSON lines, its
  table as CSV and JSON, its docs as markdown, a copy of the workspace's
  folder (`workspace/`: every channel's repository, full history and all),
  and `export-viewer.html`, which browses all of it offline.
- **Import.** *Import a workspace* on the home screen takes a folder Export
  workspace made and makes it a workspace again, in a new folder: channels,
  chat (from `chat.jsonl`), tables, docs, access and open proposals (from
  the repositories in `workspace/`, or `history.git` in an export from
  0.23). It also takes a workspace's own folder (one with `gild.json`, such
  as a clone), which it opens where it is. Imported chat keeps its authors
  and times and wakes no one. A test exports, imports and exports again,
  and checks the two exports match.
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
- `src-tauri/src/store/folders.rs`: where each workspace's folder is,
  `gild.json`, and moving data from before 0.24 into folders.
  `src-tauri/src/watch.rs`: notices files changed by other apps (with a
  scan at startup for what changed while gild was closed).
- `src-tauri/src/records.rs`: each channel's records live in its git
  repository in the workspace's folder, as
  `.gild/view.json` and one `.gild/records/<id>.json` per record. Git is the
  truth: every change is a commit by the person who made it, written to
  disk and git's index too (`git status` stays clean), and the SQLite
  tables are an index rebuilt from it (on every write, and on startup when a
  channel's history has moved past its index). Pure Rust (gitoxide): no
  `git` binary needed. Docs
  are `docs/<name>.md` in the same repository. An agent's proposed edit is a
  branch, `proposals/<id>`; approving it is a merge commit by the person who
  approved, and rejecting it moves the branch to `refs/gild/rejected/<id>`,
  so what was proposed is never lost.
- `src-tauri/src/view.rs`: the shared view and its one controller.
- `src-tauri/src/addons.rs` and `src-tauri/addons/`: add-on manifests, the
  registry, and the Markdown add-on.
- `src-tauri/src/clients.rs`: finding Claude Code, Codex and Pi on this
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
  SQLite file; `sqlite3` reads it. It holds the chat, the index, and where
  each workspace's folder is.

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

MIT.
