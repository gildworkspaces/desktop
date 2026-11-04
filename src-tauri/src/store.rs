//! The workspace's local store: workspaces, channels and messages in one
//! SQLite file in the app's data folder, written with Turso's Rust engine
//! (`turso_core`), the same engine Zega+SQLite uses. No C.
//!
//! Every read and write in the app goes through these functions; the window
//! never touches the file.

use std::num::NonZero;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use turso_core::{Connection, Database, PlatformIO, SqliteDialect, Value, IO};

use crate::records::{self, Access, Acl, Change, DiffLine, Doc, Record, View};
use sha2::{Digest, Sha256};

/// Bumped when a table's columns or meaning change.
const FORMAT: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS gild_meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS workspaces (id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS channels (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, name TEXT NOT NULL, topic TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS messages (id TEXT PRIMARY KEY, channel_id TEXT NOT NULL, author TEXT NOT NULL, kind TEXT NOT NULL, body TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS views (channel_id TEXT PRIMARY KEY, config TEXT NOT NULL, indexed_commit TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS records (channel_id TEXT NOT NULL, id TEXT NOT NULL, title TEXT NOT NULL, status TEXT NOT NULL, fields TEXT NOT NULL, created_at INTEGER NOT NULL, PRIMARY KEY (channel_id, id));
CREATE TABLE IF NOT EXISTS indexed (channel_id TEXT PRIMARY KEY, commit_id TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS grants (channel_id TEXT NOT NULL, agent_id TEXT NOT NULL, access TEXT NOT NULL, PRIMARY KEY (channel_id, agent_id));
CREATE TABLE IF NOT EXISTS agents (id TEXT PRIMARY KEY, name TEXT NOT NULL, token_sha256 TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS docs (channel_id TEXT NOT NULL, path TEXT NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL, PRIMARY KEY (channel_id, path));
CREATE TABLE IF NOT EXISTS mentions (id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, channel_id TEXT NOT NULL, message_id TEXT NOT NULL, created_at INTEGER NOT NULL, delivered_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS agent_wake (agent_id TEXT PRIMARY KEY, command TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS audit (id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, agent TEXT NOT NULL, action TEXT NOT NULL, channel_id TEXT NOT NULL, detail TEXT NOT NULL, ok INTEGER NOT NULL, at INTEGER NOT NULL);
";

/// The file name of the database inside the data folder.
pub const DB_FILE: &str = "gild.db";

/// The account everything is written as until real accounts exist.
pub const DEMO_NAME: &str = "Demo";

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub channels: i64,
    pub messages: i64,
    pub created_at: i64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Channel {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub topic: String,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Message {
    pub id: String,
    pub channel_id: String,
    pub author: String,
    /// `person` or `system`; `agent` arrives with agents.
    pub kind: String,
    pub body: String,
    pub created_at: i64,
}

/// A channel's view and records, as the index has them.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ChannelData {
    pub view: Option<View>,
    pub records: Vec<Record>,
}

pub struct Store {
    /// The data folder: the database, and `channels/<id>.git` for each
    /// channel's records (the truth the index is built from).
    data: PathBuf,
    conn: Arc<Connection>,
    // Held so the database outlives every statement on `conn`.
    _db: Arc<Database>,
    signals: Signals,
}

/// Rung when an agent is mentioned: `waiters` wakes every agent blocked in
/// `wait_for_mentions`; `runner` wakes the loop that runs agents' commands
/// (and keeps the ring if the loop is busy, so nothing is missed).
#[derive(Clone, Default)]
pub struct Signals {
    pub waiters: Arc<tokio::sync::Notify>,
    pub runner: Arc<tokio::sync::Notify>,
}

fn err(context: &str, e: impl std::fmt::Display) -> String {
    format!("{context}: {e}")
}

/// Milliseconds since the Unix epoch.
pub fn now() -> i64 {
    now_ms()
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Unique and time-ordered: milliseconds, then a per-process counter.
fn new_id() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:012x}{:06x}{:04x}", now_ms(), std::process::id() & 0xff_ffff, n & 0xffff)
}

fn text(v: &Value) -> String {
    v.to_text().unwrap_or_default().to_string()
}

fn int(v: &Value) -> i64 {
    match v {
        Value::Numeric(turso_core::Numeric::Integer(i)) => *i,
        _ => 0,
    }
}

/// A channel name as people type it, made into one: lower case, words joined
/// by `-`, only letters, digits and `-`, at most 40 characters.
pub fn channel_name(input: &str) -> Option<String> {
    let mut out = String::new();
    for c in input.trim().chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if (c == '-' || c == '_' || c.is_whitespace()) && !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(40).collect();
    let out = out.trim_end_matches('-').to_string();
    (!out.is_empty()).then_some(out)
}

impl Store {
    /// Open the store in `data`, and bring the index up to date with every
    /// channel's history.
    pub fn open(data: &Path) -> Result<Store, String> {
        let db_path = data.join(DB_FILE);
        let path = db_path.to_str().ok_or("the data folder's path is not valid UTF-8")?;
        let io: Arc<dyn IO> = Arc::new(PlatformIO::new().map_err(|e| err("starting SQLite I/O", e))?);
        let db = Database::open_file(io, path, Arc::new(SqliteDialect)).map_err(|e| err("opening the workspace database", e))?;
        let conn = db.connect().map_err(|e| err("connecting to the workspace database", e))?;
        conn.execute("PRAGMA journal_mode = wal")
            .and_then(|()| conn.execute("PRAGMA synchronous = FULL"))
            .and_then(|()| conn.execute(SCHEMA))
            .map_err(|e| err("preparing the workspace database", e))?;
        let store = Store { data: data.to_path_buf(), conn, _db: db, signals: Signals::default() };
        match store.query("SELECT value FROM gild_meta WHERE key = 'format'", &[])?.first() {
            None => store.run("INSERT INTO gild_meta (key, value) VALUES ('format', ?1)", &[Value::from_i64(FORMAT)])?,
            Some(row) if int(&row[0]) == FORMAT => {}
            Some(row) => return Err(format!("this database is format {}, and this gild reads format {FORMAT}", int(&row[0]))),
        }
        store.catch_up()?;
        Ok(store)
    }

    fn bind(statement: &mut turso_core::Statement, params: &[Value]) -> Result<(), String> {
        for (i, value) in params.iter().enumerate() {
            let index = NonZero::new(i + 1).expect("parameters are numbered from 1");
            statement.bind_at(index, value.clone()).map_err(|e| err("binding a parameter", e))?;
        }
        Ok(())
    }

    fn run(&self, sql: &str, params: &[Value]) -> Result<(), String> {
        let mut statement = self.conn.prepare(sql).map_err(|e| err("preparing a write", e))?;
        Self::bind(&mut statement, params)?;
        statement.run_ignore_rows().map_err(|e| err("writing", e))
    }

    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Vec<Value>>, String> {
        let mut statement = self.conn.prepare(sql).map_err(|e| err("preparing a read", e))?;
        Self::bind(&mut statement, params)?;
        statement.run_collect_rows().map_err(|e| err("reading", e))
    }

    /// A first run gets one workspace with #general, so the app opens on
    /// something you can use.
    pub fn seed_if_empty(&self) -> Result<(), String> {
        if !self.workspaces()?.is_empty() {
            return Ok(());
        }
        let ws = self.create_workspace("Demo workspace")?;
        let general = &self.channels(&ws.id)?[0];
        self.post(
            &general.id,
            "gild",
            "system",
            "Welcome to gild. This demo workspace lives on this computer, and everything you write here is saved locally.",
        )?;
        Ok(())
    }

    /// The data folder this store keeps its database and histories in.
    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    pub fn workspaces(&self) -> Result<Vec<Workspace>, String> {
        let rows = self.query(
            "SELECT w.id, w.name, w.created_at,
                (SELECT count(*) FROM channels c WHERE c.workspace_id = w.id),
                (SELECT count(*) FROM messages m JOIN channels c ON c.id = m.channel_id WHERE c.workspace_id = w.id)
             FROM workspaces w ORDER BY w.created_at",
            &[],
        )?;
        Ok(rows
            .iter()
            .map(|r| Workspace { id: text(&r[0]), name: text(&r[1]), created_at: int(&r[2]), channels: int(&r[3]), messages: int(&r[4]) })
            .collect())
    }

    /// A new workspace starts with #general.
    pub fn create_workspace(&self, name: &str) -> Result<Workspace, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Give the workspace a name.".into());
        }
        if name.chars().count() > 60 {
            return Err("Workspace names are at most 60 characters.".into());
        }
        let (id, at) = (new_id(), now_ms());
        self.run(
            "INSERT INTO workspaces (id, name, created_at) VALUES (?1, ?2, ?3)",
            &[Value::from_text(id.clone()), Value::from_text(name.to_string()), Value::from_i64(at)],
        )?;
        self.create_channel(&id, "general", "Anything for the whole workspace")?;
        self.workspaces()?.into_iter().find(|w| w.id == id).ok_or_else(|| "the new workspace was not saved".into())
    }

    pub fn channels(&self, workspace_id: &str) -> Result<Vec<Channel>, String> {
        let rows = self.query(
            "SELECT id, workspace_id, name, topic, created_at FROM channels WHERE workspace_id = ?1 ORDER BY created_at",
            &[Value::from_text(workspace_id.to_string())],
        )?;
        Ok(rows
            .iter()
            .map(|r| Channel { id: text(&r[0]), workspace_id: text(&r[1]), name: text(&r[2]), topic: text(&r[3]), created_at: int(&r[4]) })
            .collect())
    }

    pub fn create_channel(&self, workspace_id: &str, name: &str, topic: &str) -> Result<Channel, String> {
        let name = channel_name(name).ok_or("Give the channel a name with at least one letter or digit.")?;
        if self.query("SELECT 1 FROM workspaces WHERE id = ?1", &[Value::from_text(workspace_id.to_string())])?.is_empty() {
            return Err("That workspace no longer exists.".into());
        }
        if self.channels(workspace_id)?.iter().any(|c| c.name == name) {
            return Err(format!("#{name} already exists in this workspace."));
        }
        let channel = Channel { id: new_id(), workspace_id: workspace_id.to_string(), name, topic: topic.trim().to_string(), created_at: now_ms() };
        self.run(
            "INSERT INTO channels (id, workspace_id, name, topic, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            &[
                Value::from_text(channel.id.clone()),
                Value::from_text(channel.workspace_id.clone()),
                Value::from_text(channel.name.clone()),
                Value::from_text(channel.topic.clone()),
                Value::from_i64(channel.created_at),
            ],
        )?;
        Ok(channel)
    }

    pub fn messages(&self, channel_id: &str) -> Result<Vec<Message>, String> {
        let rows = self.query(
            "SELECT id, channel_id, author, kind, body, created_at FROM messages WHERE channel_id = ?1 ORDER BY created_at, id",
            &[Value::from_text(channel_id.to_string())],
        )?;
        Ok(rows
            .iter()
            .map(|r| Message {
                id: text(&r[0]),
                channel_id: text(&r[1]),
                author: text(&r[2]),
                kind: text(&r[3]),
                body: text(&r[4]),
                created_at: int(&r[5]),
            })
            .collect())
    }

    pub fn post(&self, channel_id: &str, author: &str, kind: &str, body: &str) -> Result<Message, String> {
        let body = body.trim();
        if body.is_empty() {
            return Err("A message needs some text.".into());
        }
        if self.query("SELECT 1 FROM channels WHERE id = ?1", &[Value::from_text(channel_id.to_string())])?.is_empty() {
            return Err("That channel no longer exists.".into());
        }
        let message = Message {
            id: new_id(),
            channel_id: channel_id.to_string(),
            author: author.to_string(),
            kind: kind.to_string(),
            body: body.to_string(),
            created_at: now_ms(),
        };
        self.run(
            "INSERT INTO messages (id, channel_id, author, kind, body, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            &[
                Value::from_text(message.id.clone()),
                Value::from_text(message.channel_id.clone()),
                Value::from_text(message.author.clone()),
                Value::from_text(message.kind.clone()),
                Value::from_text(message.body.clone()),
                Value::from_i64(message.created_at),
            ],
        )?;
        self.record_mentions(&message)?;
        Ok(message)
    }
}


/// Limits that keep a view readable.
const MAX_FIELDS: usize = 12;
const MAX_STATUSES: usize = 8;

fn clean_list(items: &[String], what: &str, max: usize) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for item in items.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if item.chars().count() > 40 {
            return Err(format!("{what} names are at most 40 characters."));
        }
        if out.iter().any(|o| o.eq_ignore_ascii_case(item)) {
            return Err(format!("\"{item}\" is listed twice."));
        }
        out.push(item.to_string());
    }
    if out.len() > max {
        return Err(format!("A table has at most {max} {what}s."));
    }
    Ok(out)
}

impl Store {
    /// Rebuild the index of every channel whose history moved past it. The
    /// index is a function of the commit, so this is the same code that
    /// indexes each change.
    fn catch_up(&self) -> Result<(), String> {
        for row in self.query("SELECT id FROM channels", &[])? {
            let channel = text(&row[0]);
            let head = records::head_commit(&self.data, &channel)?;
            let indexed = self
                .query("SELECT commit_id FROM indexed WHERE channel_id = ?1", &[Value::from_text(channel.clone())])?
                .first()
                .map(|r| text(&r[0]));
            if head != indexed {
                self.reindex(&channel)?;
            }
        }
        Ok(())
    }

    /// Replace a channel's index with what its history says now.
    fn reindex(&self, channel_id: &str) -> Result<(), String> {
        let snap = records::snapshot(&self.data, channel_id)?;
        let id = Value::from_text(channel_id.to_string());
        self.run("DELETE FROM records WHERE channel_id = ?1", std::slice::from_ref(&id))?;
        self.run("DELETE FROM views WHERE channel_id = ?1", std::slice::from_ref(&id))?;
        self.run("DELETE FROM grants WHERE channel_id = ?1", std::slice::from_ref(&id))?;
        self.run("DELETE FROM indexed WHERE channel_id = ?1", std::slice::from_ref(&id))?;
        self.run("DELETE FROM docs WHERE channel_id = ?1", std::slice::from_ref(&id))?;
        for d in &snap.docs {
            self.run(
                "INSERT INTO docs (channel_id, path, title, body) VALUES (?1, ?2, ?3, ?4)",
                &[id.clone(), Value::from_text(d.path.clone()), Value::from_text(d.title.clone()), Value::from_text(d.body.clone())],
            )?;
        }
        if let Some(commit) = &snap.commit {
            self.run("INSERT INTO indexed (channel_id, commit_id) VALUES (?1, ?2)", &[id.clone(), Value::from_text(commit.clone())])?;
        }
        for (agent, access) in &snap.acl.agents {
            let access = serde_json::to_value(access).map_err(|e| err("indexing access", e))?;
            self.run(
                "INSERT INTO grants (channel_id, agent_id, access) VALUES (?1, ?2, ?3)",
                &[id.clone(), Value::from_text(agent.clone()), Value::from_text(access.as_str().unwrap_or("read").to_string())],
            )?;
        }
        if let (Some(view), Some(commit)) = (&snap.view, &snap.commit) {
            let config = serde_json::to_string(view).map_err(|e| err("indexing the view", e))?;
            self.run(
                "INSERT INTO views (channel_id, config, indexed_commit) VALUES (?1, ?2, ?3)",
                &[id.clone(), Value::from_text(config), Value::from_text(commit.clone())],
            )?;
        }
        for r in &snap.records {
            let fields = serde_json::to_string(&r.fields).map_err(|e| err("indexing a record", e))?;
            self.run(
                "INSERT INTO records (channel_id, id, title, status, fields, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                &[
                    id.clone(),
                    Value::from_text(r.id.clone()),
                    Value::from_text(r.title.clone()),
                    Value::from_text(r.status.clone()),
                    Value::from_text(fields),
                    Value::from_i64(r.created_at),
                ],
            )?;
        }
        Ok(())
    }

    /// Commit, then index: the change is real once the commit exists, and the
    /// index follows it.
    fn write(&self, channel_id: &str, author: &str, message: &str, files: &[(String, Vec<u8>)]) -> Result<(), String> {
        records::commit(&self.data, channel_id, author, message, files)?;
        self.reindex(channel_id)
    }

    fn require_channel(&self, channel_id: &str) -> Result<(), String> {
        if self.query("SELECT 1 FROM channels WHERE id = ?1", &[Value::from_text(channel_id.to_string())])?.is_empty() {
            return Err("That channel no longer exists.".into());
        }
        Ok(())
    }

    pub fn channel_data(&self, channel_id: &str) -> Result<ChannelData, String> {
        let id = Value::from_text(channel_id.to_string());
        let view = match self.query("SELECT config FROM views WHERE channel_id = ?1", std::slice::from_ref(&id))?.first() {
            Some(row) => Some(serde_json::from_str::<View>(&text(&row[0])).map_err(|e| err("reading the view", e))?),
            None => None,
        };
        let rows = self.query(
            "SELECT id, title, status, fields, created_at FROM records WHERE channel_id = ?1 ORDER BY created_at, id",
            &[id],
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(Record {
                id: text(&r[0]),
                title: text(&r[1]),
                status: text(&r[2]),
                fields: serde_json::from_str(&text(&r[3])).map_err(|e| err("reading a record", e))?,
                created_at: int(&r[4]),
            });
        }
        Ok(ChannelData { view, records: out })
    }

    /// Give a channel a table (or change its columns and statuses).
    pub fn set_view(&self, channel_id: &str, author: &str, fields: &[String], statuses: &[String]) -> Result<ChannelData, String> {
        self.require_channel(channel_id)?;
        let fields = clean_list(fields, "column", MAX_FIELDS)?;
        let statuses = clean_list(statuses, "status", MAX_STATUSES)?;
        if statuses.is_empty() {
            return Err("A table needs at least one status, such as To do.".into());
        }
        let had = self.channel_data(channel_id)?.view.is_some();
        let view = View { kind: "table".into(), fields, statuses };
        let bytes = serde_json::to_vec_pretty(&view).map_err(|e| err("saving the view", e))?;
        let message = if had { "Change the table's columns" } else { "Add a table" };
        self.write(channel_id, author, message, &[(records::VIEW_PATH.to_string(), bytes)])?;
        self.channel_data(channel_id)
    }

    fn check_record(view: &View, title: &str, status: &str, fields: &BTreeMap<String, String>) -> Result<(), String> {
        if title.trim().is_empty() {
            return Err("A record needs a title.".into());
        }
        if title.chars().count() > 200 {
            return Err("Titles are at most 200 characters.".into());
        }
        if !view.statuses.iter().any(|s| s == status) {
            return Err(format!("\"{status}\" is not one of this table's statuses."));
        }
        for (name, value) in fields {
            if !view.fields.iter().any(|f| f == name) {
                return Err(format!("This table has no column \"{name}\"."));
            }
            if value.chars().count() > 2000 {
                return Err(format!("\"{name}\" is longer than 2,000 characters."));
            }
        }
        Ok(())
    }

    fn view_of(&self, channel_id: &str) -> Result<View, String> {
        self.channel_data(channel_id)?.view.ok_or_else(|| "This channel has no table yet.".to_string())
    }

    pub fn add_record(
        &self,
        channel_id: &str,
        author: &str,
        title: &str,
        status: &str,
        fields: BTreeMap<String, String>,
    ) -> Result<Record, String> {
        self.require_channel(channel_id)?;
        let view = self.view_of(channel_id)?;
        let fields: BTreeMap<String, String> = fields.into_iter().map(|(k, v)| (k, v.trim().to_string())).filter(|(_, v)| !v.is_empty()).collect();
        Self::check_record(&view, title, status, &fields)?;
        let record = Record { id: new_id(), title: title.trim().to_string(), status: status.to_string(), fields, created_at: now_ms() };
        let bytes = serde_json::to_vec_pretty(&record).map_err(|e| err("saving the record", e))?;
        self.write(channel_id, author, &format!("Add {}", record.title), &[(records::record_path(&record.id), bytes)])?;
        Ok(record)
    }

    /// Change a record. The commit message says what changed, in words.
    pub fn update_record(
        &self,
        channel_id: &str,
        author: &str,
        record_id: &str,
        title: &str,
        status: &str,
        fields: BTreeMap<String, String>,
    ) -> Result<Record, String> {
        let data = self.channel_data(channel_id)?;
        let view = data.view.ok_or("This channel has no table yet.")?;
        let old = data.records.into_iter().find(|r| r.id == record_id).ok_or("That record no longer exists.")?;
        let fields: BTreeMap<String, String> = fields.into_iter().map(|(k, v)| (k, v.trim().to_string())).filter(|(_, v)| !v.is_empty()).collect();
        Self::check_record(&view, title, status, &fields)?;
        let new = Record { id: old.id.clone(), title: title.trim().to_string(), status: status.to_string(), fields, created_at: old.created_at };
        let mut said = Vec::new();
        if new.title != old.title {
            said.push(format!("renamed from {}", old.title));
        }
        if new.status != old.status {
            said.push(format!("{} → {}", old.status, new.status));
        }
        for name in &view.fields {
            let (a, b) = (old.fields.get(name), new.fields.get(name));
            if a != b {
                said.push(match b {
                    Some(v) => format!("{name}: {v}"),
                    None => format!("{name} cleared"),
                });
            }
        }
        if said.is_empty() {
            return Ok(old);
        }
        let bytes = serde_json::to_vec_pretty(&new).map_err(|e| err("saving the record", e))?;
        let message = format!("{}: {}", new.title, said.join(", "));
        self.write(channel_id, author, &message, &[(records::record_path(&new.id), bytes)])?;
        Ok(new)
    }

    /// Every change to one record, newest first, from git.
    pub fn record_history(&self, channel_id: &str, record_id: &str) -> Result<Vec<Change>, String> {
        records::history(&self.data, channel_id, &records::record_path(record_id))
    }
}


/// An agent's proposed doc edit, as the window and agents see it.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Proposal {
    pub id: String,
    pub agent: String,
    pub summary: String,
    pub at: i64,
    pub path: String,
    pub title: String,
    /// The doc doesn't exist yet: approving creates it.
    pub new_doc: bool,
    /// The doc changed after the proposal: it can only be rejected.
    pub stale: bool,
    pub diff: Vec<DiffLine>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ChannelDocs {
    pub docs: Vec<Doc>,
    pub proposals: Vec<Proposal>,
}

/// A part of a doc that matches a search: the doc, the heading it's under,
/// and the lines that matched.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct DocHit {
    pub channel_id: String,
    pub channel: String,
    pub path: String,
    pub title: String,
    pub section: String,
    pub excerpt: String,
    pub score: usize,
}

/// Docs are at most this many characters: a long SOP, not a book.
const MAX_DOC: usize = 200_000;

/// Words a question is made of that say nothing about what it's after.
const STOP: &[&str] = &[
    "the", "and", "for", "are", "was", "were", "what", "where", "when", "which", "who", "whom", "why", "how", "does", "did", "doing",
    "with", "from", "into", "that", "this", "these", "those", "there", "their", "they", "them", "you", "your", "our", "can", "should",
    "would", "could", "will", "about", "have", "has", "had", "not", "but", "any", "all", "get", "got", "goes", "go", "put", "need",
];

/// A search's terms: lower case, without stop words, without a plural `s`.
fn terms(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in query.split(|c: char| !c.is_alphanumeric()).map(str::to_lowercase) {
        if w.chars().count() < 3 || STOP.contains(&w.as_str()) {
            continue;
        }
        let w = if w.chars().count() > 3 && w.ends_with('s') && !w.ends_with("ss") { w[..w.len() - 1].to_string() } else { w };
        if !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

/// The parts of one doc that match `terms`, each scored by how many terms
/// it has (a term in the heading counts twice).
fn search_doc(doc: &Doc, terms: &[String]) -> Vec<(String, String, usize)> {
    let mut sections: Vec<(String, Vec<&str>)> = vec![(doc.title.clone(), vec![])];
    for line in doc.body.lines() {
        let t = line.trim_start();
        if t.starts_with('#') {
            let heading = t.trim_start_matches('#').trim();
            if heading != doc.title {
                sections.push((heading.to_string(), vec![]));
            }
        } else if !t.is_empty() {
            sections.last_mut().expect("there is always a first section").1.push(line.trim());
        }
    }
    let mut out = Vec::new();
    for (heading, lines) in sections {
        // A heading with nothing under it answers nothing.
        if lines.is_empty() {
            continue;
        }
        let (h, text) = (heading.to_lowercase(), lines.join("\n").to_lowercase());
        let score: usize = terms.iter().map(|t| usize::from(text.contains(t.as_str())) + 2 * usize::from(h.contains(t.as_str()))).sum();
        if score == 0 {
            continue;
        }
        let matching: Vec<&str> = lines.iter().copied().filter(|l| terms.iter().any(|t| l.to_lowercase().contains(t.as_str()))).collect();
        let excerpt = if matching.is_empty() { lines.clone() } else { matching }.join("\n");
        let excerpt: String = excerpt.chars().take(400).collect();
        out.push((heading, excerpt, score));
    }
    out
}

impl Store {
    pub fn docs(&self, channel_id: &str) -> Result<Vec<Doc>, String> {
        let rows = self.query("SELECT path, body FROM docs WHERE channel_id = ?1 ORDER BY title, path", &[Value::from_text(channel_id.to_string())])?;
        Ok(rows.iter().map(|r| Doc::new(&text(&r[0]), text(&r[1]))).collect())
    }

    pub fn doc(&self, channel_id: &str, path: &str) -> Result<Doc, String> {
        self.docs(channel_id)?.into_iter().find(|d| d.path == path).ok_or_else(|| "There is no doc at that path in this channel.".into())
    }

    /// A channel's docs and the proposals waiting on them.
    pub fn channel_docs(&self, channel_id: &str) -> Result<ChannelDocs, String> {
        self.require_channel(channel_id)?;
        Ok(ChannelDocs { docs: self.docs(channel_id)?, proposals: self.proposals(channel_id)? })
    }

    fn check_body(body: &str) -> Result<(), String> {
        if body.chars().count() > MAX_DOC {
            return Err("Docs are at most 200,000 characters.".into());
        }
        Ok(())
    }

    /// Where a doc called `title` lives: `docs/<name>.md`.
    fn path_for(title: &str) -> Result<String, String> {
        let name = channel_name(title).ok_or("Give the doc a title with at least one letter or digit.")?;
        Ok(format!("{}/{name}.md", records::DOCS_DIR))
    }

    /// A new doc, starting with its title as a heading.
    pub fn create_doc(&self, channel_id: &str, author: &str, title: &str) -> Result<Doc, String> {
        self.require_channel(channel_id)?;
        let title = title.trim();
        if title.chars().count() > 120 {
            return Err("Doc titles are at most 120 characters.".into());
        }
        let path = Self::path_for(title)?;
        if self.docs(channel_id)?.iter().any(|d| d.path == path) {
            return Err(format!("This channel already has a doc at {path}."));
        }
        let body = format!("# {title}\n\n");
        self.write(channel_id, author, &format!("Add doc {title}"), &[(path.clone(), body.into_bytes())])?;
        self.doc(channel_id, &path)
    }

    /// Save a person's edit to a doc. Nothing changed: no commit.
    pub fn save_doc(&self, channel_id: &str, author: &str, path: &str, body: &str) -> Result<Doc, String> {
        let old = self.doc(channel_id, path)?;
        Self::check_body(body)?;
        if old.body == body {
            return Ok(old);
        }
        self.write(channel_id, author, &format!("Edit {}", Doc::new(path, body.to_string()).title), &[(path.to_string(), body.as_bytes().to_vec())])?;
        self.doc(channel_id, path)
    }

    pub fn doc_history(&self, channel_id: &str, path: &str) -> Result<Vec<Change>, String> {
        records::history(&self.data, channel_id, path)
    }

    pub fn proposals(&self, channel_id: &str) -> Result<Vec<Proposal>, String> {
        Ok(records::proposals(&self.data, channel_id)?
            .into_iter()
            .map(|p| Proposal {
                title: Doc::new(&p.path, p.after.clone()).title,
                diff: records::diff(p.before.as_deref().unwrap_or(""), &p.after),
                new_doc: p.before.is_none(),
                id: p.id,
                agent: p.author,
                summary: p.message,
                at: p.at,
                path: p.path,
                stale: p.stale,
            })
            .collect())
    }

    /// An agent's edit to a doc (`path`), or a new doc (`title`), as a
    /// proposal: the doc doesn't change until a person approves it.
    pub fn propose_doc(&self, channel_id: &str, agent: &str, path: Option<&str>, title: Option<&str>, body: &str, summary: &str) -> Result<Proposal, String> {
        self.require_channel(channel_id)?;
        Self::check_body(body)?;
        let summary = summary.trim();
        if summary.is_empty() {
            return Err("Say in a few words what the change is and why (`summary`).".into());
        }
        let (path, title) = match (path, title) {
            (Some(p), _) => {
                let d = self.doc(channel_id, p)?;
                if d.body == body {
                    return Err("That is what the doc already says.".into());
                }
                (d.path, d.title)
            }
            (None, Some(t)) => {
                let p = Self::path_for(t)?;
                if self.docs(channel_id)?.iter().any(|d| d.path == p) {
                    return Err(format!("A doc already exists at {p}: propose an edit to it with `path`."));
                }
                (p, t.trim().to_string())
            }
            (None, None) => return Err("Give `path` to edit a doc, or `title` for a new one.".into()),
        };
        let id = new_id();
        records::propose(&self.data, channel_id, &id, agent, summary, &path, body.as_bytes())?;
        self.post(channel_id, "gild", "system", &format!("{agent} proposed a change to {title}: {summary}. It waits for approval in Docs."))?;
        self.proposals(channel_id)?.into_iter().find(|p| p.id == id).ok_or_else(|| "the proposal was not saved".into())
    }

    fn proposal(&self, channel_id: &str, id: &str) -> Result<Proposal, String> {
        self.proposals(channel_id)?.into_iter().find(|p| p.id == id).ok_or_else(|| "That proposal was already approved or rejected.".into())
    }

    pub fn approve_proposal(&self, channel_id: &str, author: &str, id: &str) -> Result<(), String> {
        let p = self.proposal(channel_id, id)?;
        let message = format!("Approve {}'s change to {}: {}", p.agent, p.title, p.summary);
        records::approve(&self.data, channel_id, id, author, &message)?;
        self.reindex(channel_id)?;
        self.post(channel_id, "gild", "system", &format!("{author} approved {}'s change to {}.", p.agent, p.title))?;
        Ok(())
    }

    pub fn reject_proposal(&self, channel_id: &str, author: &str, id: &str) -> Result<(), String> {
        let p = self.proposal(channel_id, id)?;
        records::reject(&self.data, channel_id, id)?;
        self.post(channel_id, "gild", "system", &format!("{author} rejected {}'s change to {}.", p.agent, p.title))?;
        Ok(())
    }

    /// The parts of these channels' docs that best match `query`, best first.
    pub fn search_docs(&self, channel_ids: &[String], query: &str) -> Result<Vec<DocHit>, String> {
        let terms = terms(query);
        if terms.is_empty() {
            return Err("Search for a few words that would be in the doc.".into());
        }
        let mut hits = Vec::new();
        for id in channel_ids {
            let Some(channel) = self.channel(id)? else { continue };
            for doc in self.docs(id)? {
                for (section, excerpt, score) in search_doc(&doc, &terms) {
                    hits.push(DocHit { channel_id: id.clone(), channel: channel.name.clone(), path: doc.path.clone(), title: doc.title.clone(), section, excerpt, score });
                }
            }
        }
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| (&a.channel, &a.path).cmp(&(&b.channel, &b.path))));
        hits.truncate(5);
        Ok(hits)
    }
}

/// An agent: a member that works in the channels it's given, through the
/// app's MCP endpoint, with its own token.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub created_at: i64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct AgentAccess {
    pub agent: Agent,
    pub access: Option<Access>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct AgentChannel {
    pub workspace: String,
    pub channel: Channel,
    pub access: Access,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct AuditEntry {
    pub agent: String,
    pub action: String,
    pub detail: String,
    pub ok: bool,
    pub at: i64,
}

fn sha256_hex(s: &str) -> String {
    Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_access(s: &str) -> Option<Access> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
}

impl Store {
    pub fn agents(&self) -> Result<Vec<Agent>, String> {
        let rows = self.query("SELECT id, name, created_at FROM agents ORDER BY created_at", &[])?;
        Ok(rows.iter().map(|r| Agent { id: text(&r[0]), name: text(&r[1]), created_at: int(&r[2]) }).collect())
    }

    /// A new agent and its token. The token is shown once; only its hash is
    /// kept.
    pub fn create_agent(&self, name: &str) -> Result<(Agent, String), String> {
        let name = channel_name(name).ok_or("Give the agent a name with at least one letter or digit.")?;
        if self.agents()?.iter().any(|a| a.name == name) {
            return Err(format!("There is already an agent called {name}."));
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| err("making a token", e))?;
        let token = format!("gild_{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>());
        let agent = Agent { id: new_id(), name, created_at: now_ms() };
        self.run(
            "INSERT INTO agents (id, name, token_sha256, created_at) VALUES (?1, ?2, ?3, ?4)",
            &[
                Value::from_text(agent.id.clone()),
                Value::from_text(agent.name.clone()),
                Value::from_text(sha256_hex(&token)),
                Value::from_i64(agent.created_at),
            ],
        )?;
        Ok((agent, token))
    }

    pub fn agent_for_token(&self, token: &str) -> Result<Option<Agent>, String> {
        let rows = self.query(
            "SELECT id, name, created_at FROM agents WHERE token_sha256 = ?1",
            &[Value::from_text(sha256_hex(token.trim()))],
        )?;
        Ok(rows.first().map(|r| Agent { id: text(&r[0]), name: text(&r[1]), created_at: int(&r[2]) }))
    }

    pub fn access(&self, agent_id: &str, channel_id: &str) -> Result<Option<Access>, String> {
        let rows = self.query(
            "SELECT access FROM grants WHERE channel_id = ?1 AND agent_id = ?2",
            &[Value::from_text(channel_id.to_string()), Value::from_text(agent_id.to_string())],
        )?;
        Ok(rows.first().and_then(|r| parse_access(&text(&r[0]))))
    }

    /// Refuse unless the agent has at least `need` in the channel. The answer
    /// never says whether a channel it can't see exists.
    pub fn require(&self, agent: &Agent, channel_id: &str, need: Access) -> Result<(), String> {
        match self.access(&agent.id, channel_id)? {
            Some(have) if have >= need => Ok(()),
            Some(_) => Err(format!("{} can read this channel but not change it.", agent.name)),
            None => Err("No channel with that id is shared with this agent.".into()),
        }
    }

    /// Every agent with what it may do in this channel, for the window.
    pub fn channel_access(&self, channel_id: &str) -> Result<Vec<AgentAccess>, String> {
        self.agents()?
            .into_iter()
            .map(|agent| Ok(AgentAccess { access: self.access(&agent.id, channel_id)?, agent }))
            .collect()
    }

    /// Give, change or take away an agent's access to a channel: a commit to
    /// the channel's `.gild/acl.json`, so every grant is in its history.
    pub fn set_access(&self, channel_id: &str, author: &str, agent_id: &str, access: Option<Access>) -> Result<(), String> {
        self.require_channel(channel_id)?;
        let agent = self.agents()?.into_iter().find(|a| a.id == agent_id).ok_or("That agent no longer exists.")?;
        let mut acl = Acl::default();
        for row in self.query("SELECT agent_id, access FROM grants WHERE channel_id = ?1", &[Value::from_text(channel_id.to_string())])? {
            if let Some(a) = parse_access(&text(&row[1])) {
                acl.agents.insert(text(&row[0]), a);
            }
        }
        if acl.agents.get(agent_id).copied() == access {
            return Ok(());
        }
        let message = match access {
            Some(Access::Write) => format!("Let {} read and change this channel", agent.name),
            Some(Access::Read) => format!("Let {} read this channel", agent.name),
            None => format!("Remove {}'s access", agent.name),
        };
        match access {
            Some(a) => acl.agents.insert(agent_id.to_string(), a),
            None => acl.agents.remove(agent_id),
        };
        let bytes = serde_json::to_vec_pretty(&acl).map_err(|e| err("saving access", e))?;
        self.write(channel_id, author, &message, &[(records::ACL_PATH.to_string(), bytes)])
    }

    /// The channels shared with an agent, with what it may do in each.
    pub fn agent_channels(&self, agent: &Agent) -> Result<Vec<AgentChannel>, String> {
        let rows = self.query(
            "SELECT w.name, c.id, c.workspace_id, c.name, c.topic, c.created_at, g.access
             FROM grants g JOIN channels c ON c.id = g.channel_id JOIN workspaces w ON w.id = c.workspace_id
             WHERE g.agent_id = ?1 ORDER BY w.created_at, c.created_at",
            &[Value::from_text(agent.id.clone())],
        )?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                Some(AgentChannel {
                    workspace: text(&r[0]),
                    channel: Channel { id: text(&r[1]), workspace_id: text(&r[2]), name: text(&r[3]), topic: text(&r[4]), created_at: int(&r[5]) },
                    access: parse_access(&text(&r[6]))?,
                })
            })
            .collect())
    }

    pub fn channel(&self, channel_id: &str) -> Result<Option<Channel>, String> {
        let rows = self.query(
            "SELECT id, workspace_id, name, topic, created_at FROM channels WHERE id = ?1",
            &[Value::from_text(channel_id.to_string())],
        )?;
        Ok(rows.first().map(|r| Channel { id: text(&r[0]), workspace_id: text(&r[1]), name: text(&r[2]), topic: text(&r[3]), created_at: int(&r[4]) }))
    }

    /// Record what an agent did or tried to do.
    pub fn audit(&self, agent: &Agent, action: &str, channel_id: &str, detail: &str, ok: bool) -> Result<(), String> {
        self.run(
            "INSERT INTO audit (id, agent_id, agent, action, channel_id, detail, ok, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            &[
                Value::from_text(new_id()),
                Value::from_text(agent.id.clone()),
                Value::from_text(agent.name.clone()),
                Value::from_text(action.to_string()),
                Value::from_text(channel_id.to_string()),
                Value::from_text(detail.chars().take(300).collect::<String>()),
                Value::from_i64(ok as i64),
                Value::from_i64(now_ms()),
            ],
        )
    }

    /// What agents did in a channel, newest first.
    pub fn activity(&self, channel_id: &str) -> Result<Vec<AuditEntry>, String> {
        let rows = self.query(
            "SELECT agent, action, detail, ok, at FROM audit WHERE channel_id = ?1 ORDER BY at DESC, id DESC LIMIT 200",
            &[Value::from_text(channel_id.to_string())],
        )?;
        Ok(rows
            .iter()
            .map(|r| AuditEntry { agent: text(&r[0]), action: text(&r[1]), detail: text(&r[2]), ok: int(&r[3]) != 0, at: int(&r[4]) })
            .collect())
    }
}

/// A channel coming back from an export: its chat, and its history (the
/// truth its table, docs and access are rebuilt from).
pub struct ImportChannel {
    pub name: String,
    pub topic: String,
    pub messages: Vec<Message>,
    pub history: Option<PathBuf>,
}

impl Store {
    /// A new workspace from an export. Everything is checked before anything
    /// is written. Messages keep their authors and times, and wake no one:
    /// they were said before.
    pub fn import_workspace(&self, name: &str, channels: Vec<ImportChannel>) -> Result<Workspace, String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 60 {
            return Err("The export's workspace name is missing or longer than 60 characters.".into());
        }
        let mut seen = Vec::new();
        for c in &channels {
            if channel_name(&c.name).as_deref() != Some(c.name.as_str()) {
                return Err(format!("\"{}\" isn't a channel name gild makes.", c.name));
            }
            if seen.contains(&c.name) {
                return Err(format!("#{} is in the export twice.", c.name));
            }
            seen.push(c.name.clone());
            if let Some(m) = c.messages.iter().find(|m| !["person", "system", "agent"].contains(&m.kind.as_str()) || m.body.trim().is_empty()) {
                return Err(format!("A message in #{} has no text or an unknown kind ({}).", c.name, m.kind));
            }
            if let Some(h) = &c.history {
                gix::open(h).map_err(|e| format!("#{}'s history.git isn't a git repository: {e}", c.name))?;
            }
        }
        let (ws_id, at) = (new_id(), now_ms());
        self.run(
            "INSERT INTO workspaces (id, name, created_at) VALUES (?1, ?2, ?3)",
            &[Value::from_text(ws_id.clone()), Value::from_text(name.to_string()), Value::from_i64(at)],
        )?;
        for (i, c) in channels.into_iter().enumerate() {
            let id = new_id();
            if let Some(h) = &c.history {
                crate::export::copy_dir(h, &records::repo_path(&self.data, &id))?;
            }
            // Channels are listed by when they were made: keep the export's order.
            let created = at + i as i64;
            self.run(
                "INSERT INTO channels (id, workspace_id, name, topic, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                &[
                    Value::from_text(id.clone()),
                    Value::from_text(ws_id.clone()),
                    Value::from_text(c.name.clone()),
                    Value::from_text(c.topic.trim().to_string()),
                    Value::from_i64(created),
                ],
            )?;
            for m in &c.messages {
                self.run(
                    "INSERT INTO messages (id, channel_id, author, kind, body, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    &[
                        Value::from_text(new_id()),
                        Value::from_text(id.clone()),
                        Value::from_text(m.author.clone()),
                        Value::from_text(m.kind.clone()),
                        Value::from_text(m.body.clone()),
                        Value::from_i64(m.created_at),
                    ],
                )?;
            }
            self.reindex(&id)?;
        }
        self.workspaces()?.into_iter().find(|w| w.id == ws_id).ok_or_else(|| "the imported workspace was not saved".into())
    }
}

/// A mention waiting for its agent, with what the agent needs to act on it.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Mention {
    pub id: String,
    pub agent_id: String,
    pub channel_id: String,
    pub channel: String,
    pub workspace: String,
    pub from: String,
    pub text: String,
    pub message_id: String,
    pub at: i64,
}

/// An agent as the window lists it: with the command that wakes it, if any.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct AgentInfo {
    #[serde(flatten)]
    pub agent: Agent,
    pub wake: Option<String>,
}

/// The names after `@` in a message (`@intake`, `@front-desk`), lower case,
/// each once. An `@` inside a word (an email address) doesn't count.
pub fn mentioned_names(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c != '@' || (i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '.')) {
            continue;
        }
        let name: String = chars[i + 1..].iter().take_while(|c| c.is_alphanumeric() || **c == '-' || **c == '_').collect();
        if let Some(name) = channel_name(&name) {
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

const MAX_WAKE_COMMAND: usize = 1000;

impl Store {
    /// What rings when an agent is mentioned.
    pub fn signals(&self) -> Signals {
        self.signals.clone()
    }

    /// Note who a message mentions. An agent the channel isn't shared with
    /// isn't told, and gild says so in the channel.
    fn record_mentions(&self, message: &Message) -> Result<(), String> {
        let names = mentioned_names(&message.body);
        if names.is_empty() {
            return Ok(());
        }
        let agents = self.agents()?;
        let mut told = false;
        let mut unshared = Vec::new();
        for name in names {
            let Some(agent) = agents.iter().find(|a| a.name == name) else { continue };
            if message.kind == "agent" && message.author == agent.name {
                continue;
            }
            if self.access(&agent.id, &message.channel_id)?.is_none() {
                unshared.push(agent.name.clone());
                continue;
            }
            self.run(
                "INSERT INTO mentions (id, agent_id, channel_id, message_id, created_at, delivered_at) VALUES (?1, ?2, ?3, ?4, ?5, 0)",
                &[
                    Value::from_text(new_id()),
                    Value::from_text(agent.id.clone()),
                    Value::from_text(message.channel_id.clone()),
                    Value::from_text(message.id.clone()),
                    Value::from_i64(message.created_at),
                ],
            )?;
            told = true;
        }
        for name in unshared {
            self.post(&message.channel_id, "gild", "system", &format!("{name} can't see this channel, so it wasn't told. Share the channel with {name} from Access."))?;
        }
        if told {
            self.signals.waiters.notify_waiters();
            self.signals.runner.notify_one();
        }
        Ok(())
    }

    /// An agent's mentions that haven't reached it yet, oldest first.
    pub fn pending_mentions(&self, agent_id: &str) -> Result<Vec<Mention>, String> {
        let rows = self.query(
            "SELECT n.id, n.agent_id, n.channel_id, c.name, w.name, m.author, m.body, m.id, m.created_at
             FROM mentions n JOIN messages m ON m.id = n.message_id JOIN channels c ON c.id = n.channel_id JOIN workspaces w ON w.id = c.workspace_id
             WHERE n.agent_id = ?1 AND n.delivered_at = 0 ORDER BY n.created_at, n.id",
            &[Value::from_text(agent_id.to_string())],
        )?;
        Ok(rows
            .iter()
            .map(|r| Mention {
                id: text(&r[0]),
                agent_id: text(&r[1]),
                channel_id: text(&r[2]),
                channel: text(&r[3]),
                workspace: text(&r[4]),
                from: text(&r[5]),
                text: text(&r[6]),
                message_id: text(&r[7]),
                at: int(&r[8]),
            })
            .collect())
    }

    /// Hand an agent its waiting mentions: they won't be handed out again.
    /// A mention in a channel that is no longer shared with it is dropped.
    pub fn take_mentions(&self, agent: &Agent) -> Result<Vec<Mention>, String> {
        let mut out = Vec::new();
        for m in self.pending_mentions(&agent.id)? {
            self.run("UPDATE mentions SET delivered_at = ?1 WHERE id = ?2", &[Value::from_i64(now_ms()), Value::from_text(m.id.clone())])?;
            if self.access(&agent.id, &m.channel_id)?.is_some() {
                out.push(m);
            }
        }
        Ok(out)
    }

    /// Agents with a command to run when mentioned, and mentions waiting.
    pub fn agents_to_wake(&self) -> Result<Vec<(Agent, String)>, String> {
        let rows = self.query(
            "SELECT a.id, a.name, a.created_at, k.command FROM agents a JOIN agent_wake k ON k.agent_id = a.id
             WHERE EXISTS (SELECT 1 FROM mentions n WHERE n.agent_id = a.id AND n.delivered_at = 0) ORDER BY a.created_at",
            &[],
        )?;
        Ok(rows.iter().map(|r| (Agent { id: text(&r[0]), name: text(&r[1]), created_at: int(&r[2]) }, text(&r[3]))).collect())
    }

    pub fn agent_infos(&self) -> Result<Vec<AgentInfo>, String> {
        let rows = self.query(
            "SELECT a.id, a.name, a.created_at, k.command FROM agents a LEFT JOIN agent_wake k ON k.agent_id = a.id ORDER BY a.created_at",
            &[],
        )?;
        Ok(rows
            .iter()
            .map(|r| AgentInfo {
                agent: Agent { id: text(&r[0]), name: text(&r[1]), created_at: int(&r[2]) },
                wake: match &r[3] {
                    Value::Null => None,
                    v => Some(text(v)),
                },
            })
            .collect())
    }

    /// Set (or, empty, remove) the command gild runs when the agent is
    /// mentioned. Only the window calls this: an agent can't set what runs
    /// on this computer.
    pub fn set_wake(&self, agent_id: &str, command: &str) -> Result<(), String> {
        if !self.agents()?.iter().any(|a| a.id == agent_id) {
            return Err("That agent no longer exists.".into());
        }
        let command = command.trim();
        if command.chars().count() > MAX_WAKE_COMMAND {
            return Err("The command is at most 1,000 characters.".into());
        }
        let id = Value::from_text(agent_id.to_string());
        self.run("DELETE FROM agent_wake WHERE agent_id = ?1", std::slice::from_ref(&id))?;
        if !command.is_empty() {
            self.run("INSERT INTO agent_wake (agent_id, command) VALUES (?1, ?2)", &[id, Value::from_text(command.to_string())])?;
            // Mentions that were waiting for a way to wake it.
            self.signals.runner.notify_one();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("gild-store-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), dir)
    }

    fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn records_are_rebuilt_from_git_when_the_index_is_gone() {
        let (dir, file) = temp();
        let channel = {
            let store = Store::open(&file).unwrap();
            let ws = store.create_workspace("Front Desk").unwrap();
            let c = store.create_channel(&ws.id, "student-intake", "").unwrap();
            store.set_view(&c.id, DEMO_NAME, &strings(&["Sent by", "Program"]), &strings(&["New", "Docs needed", "Enrolled"])).unwrap();
            let jordan = store.add_record(&c.id, DEMO_NAME, "Jordan Lee", "New", fields(&[("Sent by", "Northside"), ("Program", "Web Dev")])).unwrap();
            store.add_record(&c.id, DEMO_NAME, "Priya Natarajan", "Enrolled", fields(&[("Sent by", "Northside")])).unwrap();
            store.update_record(&c.id, DEMO_NAME, &jordan.id, "Jordan Lee", "Docs needed", fields(&[("Sent by", "Northside"), ("Program", "Web Dev")])).unwrap();
            c
        };
        // Lose the index: every indexed view and record.
        {
            let store = Store::open(&file).unwrap();
            store.run("DELETE FROM views", &[]).unwrap();
            store.run("DELETE FROM records", &[]).unwrap();
            store.run("DELETE FROM indexed", &[]).unwrap();
            assert!(store.channel_data(&channel.id).unwrap().records.is_empty());
        }
        // Reopening catches up from git.
        let store = Store::open(&file).unwrap();
        let data = store.channel_data(&channel.id).unwrap();
        assert_eq!(data.view.unwrap().statuses, ["New", "Docs needed", "Enrolled"]);
        let got: Vec<(String, String)> = data.records.iter().map(|r| (r.title.clone(), r.status.clone())).collect();
        assert_eq!(got, [("Jordan Lee".to_string(), "Docs needed".to_string()), ("Priya Natarajan".to_string(), "Enrolled".to_string())]);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_record_s_history_says_what_changed() {
        let (dir, file) = temp();
        let store = Store::open(&file).unwrap();
        let ws = store.create_workspace("W").unwrap();
        let c = &store.channels(&ws.id).unwrap()[0];
        store.set_view(&c.id, DEMO_NAME, &strings(&["Program"]), &strings(&["New", "Enrolled"])).unwrap();
        let r = store.add_record(&c.id, DEMO_NAME, "Ana Souza", "New", fields(&[])).unwrap();
        let other = store.add_record(&c.id, DEMO_NAME, "Tom Okafor", "New", fields(&[])).unwrap();
        store.update_record(&c.id, DEMO_NAME, &r.id, "Ana Souza", "Enrolled", fields(&[("Program", "UX")])).unwrap();
        store.update_record(&c.id, DEMO_NAME, &other.id, "Tom Okafor", "Enrolled", fields(&[])).unwrap();
        // Saving with nothing changed makes no commit.
        store.update_record(&c.id, DEMO_NAME, &r.id, "Ana Souza", "Enrolled", fields(&[("Program", "UX")])).unwrap();
        let history: Vec<String> = store.record_history(&c.id, &r.id).unwrap().into_iter().map(|h| h.message).collect();
        assert_eq!(history, ["Ana Souza: New → Enrolled, Program: UX", "Add Ana Souza"], "only this record's commits, newest first");
        assert_eq!(store.record_history(&c.id, &r.id).unwrap()[0].author, DEMO_NAME);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn records_refuse_what_they_should() {
        let (dir, file) = temp();
        let store = Store::open(&file).unwrap();
        let ws = store.create_workspace("W").unwrap();
        let c = &store.channels(&ws.id).unwrap()[0];
        assert!(store.add_record(&c.id, DEMO_NAME, "x", "New", fields(&[])).unwrap_err().contains("no table"));
        assert!(store.set_view(&c.id, DEMO_NAME, &strings(&["A", "a"]), &strings(&["New"])).unwrap_err().contains("twice"));
        assert!(store.set_view(&c.id, DEMO_NAME, &strings(&["A"]), &strings(&[" "])).is_err());
        store.set_view(&c.id, DEMO_NAME, &strings(&["A"]), &strings(&["New"])).unwrap();
        assert!(store.add_record(&c.id, DEMO_NAME, " ", "New", fields(&[])).is_err());
        assert!(store.add_record(&c.id, DEMO_NAME, "x", "Done", fields(&[])).unwrap_err().contains("statuses"));
        assert!(store.add_record(&c.id, DEMO_NAME, "x", "New", fields(&[("B", "1")])).unwrap_err().contains("no column"));
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn everything_written_is_there_after_reopening() {
        let (dir, file) = temp();
        let (ws_id, channel_id) = {
            let store = Store::open(&file).unwrap();
            store.seed_if_empty().unwrap();
            let ws = store.create_workspace("Front Desk").unwrap();
            let intake = store.create_channel(&ws.id, "Student Intake", "Students sent by partner schools").unwrap();
            store.post(&intake.id, DEMO_NAME, "person", "Northside sent three more students.").unwrap();
            (ws.id, intake.id)
        };
        let store = Store::open(&file).unwrap();
        store.seed_if_empty().unwrap();
        let names: Vec<String> = store.workspaces().unwrap().into_iter().map(|w| w.name).collect();
        assert_eq!(names, ["Demo workspace", "Front Desk"], "the seed runs once, and nothing is lost");
        let channels: Vec<String> = store.channels(&ws_id).unwrap().into_iter().map(|c| c.name).collect();
        assert_eq!(channels, ["general", "student-intake"]);
        let messages = store.messages(&channel_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].body, "Northside sent three more students.");
        assert_eq!(messages[0].author, DEMO_NAME);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn counts_follow_what_was_written() {
        let (dir, file) = temp();
        let store = Store::open(&file).unwrap();
        let ws = store.create_workspace("Counts").unwrap();
        let general = &store.channels(&ws.id).unwrap()[0];
        store.post(&general.id, DEMO_NAME, "person", "one").unwrap();
        store.post(&general.id, DEMO_NAME, "person", "two").unwrap();
        store.create_channel(&ws.id, "ideas", "").unwrap();
        let ws = store.workspaces().unwrap().into_iter().find(|w| w.id == ws.id).unwrap();
        assert_eq!((ws.channels, ws.messages), (2, 2));
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_what_it_should() {
        let (dir, file) = temp();
        let store = Store::open(&file).unwrap();
        assert!(store.create_workspace("   ").is_err());
        let ws = store.create_workspace("W").unwrap();
        assert!(store.create_channel(&ws.id, "general", "").unwrap_err().contains("already exists"));
        assert!(store.create_channel(&ws.id, "!!!", "").is_err());
        assert!(store.create_channel("nope", "x", "").is_err());
        let general = &store.channels(&ws.id).unwrap()[0];
        assert!(store.post(&general.id, DEMO_NAME, "person", "  ").is_err());
        assert!(store.post("nope", DEMO_NAME, "person", "hi").is_err());
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    const SOP: &str = "# Intake SOP\n\nHow the front desk takes in a student.\n\n## Step 1: Check in\n\nGreet the student and find them in #student-intake.\n\n## Step 2: Forms\n\nSigned intake forms go in the blue folder at the front desk.\nScan them before 5pm.\n";

    #[test]
    fn docs_are_files_in_the_channel_history() {
        let (dir, file) = temp();
        let channel = {
            let store = Store::open(&file).unwrap();
            let ws = store.create_workspace("Front Desk").unwrap();
            let c = store.channels(&ws.id).unwrap()[0].clone();
            let doc = store.create_doc(&c.id, DEMO_NAME, "Intake SOP").unwrap();
            assert_eq!((doc.path.as_str(), doc.title.as_str()), ("docs/intake-sop.md", "Intake SOP"));
            assert!(store.create_doc(&c.id, DEMO_NAME, "intake sop").unwrap_err().contains("already has"));
            store.save_doc(&c.id, DEMO_NAME, &doc.path, SOP).unwrap();
            store.save_doc(&c.id, DEMO_NAME, &doc.path, SOP).unwrap();
            assert!(store.save_doc(&c.id, DEMO_NAME, "docs/../../x.md", "x").is_err(), "only docs that exist");
            let log: Vec<String> = store.doc_history(&c.id, &doc.path).unwrap().into_iter().map(|h| h.message).collect();
            assert_eq!(log, ["Edit Intake SOP", "Add doc Intake SOP"], "saving with nothing changed makes no commit");
            store.run("DELETE FROM docs", &[]).unwrap();
            store.run("DELETE FROM indexed", &[]).unwrap();
            c
        };
        let store = Store::open(&file).unwrap();
        let docs = store.docs(&channel.id).unwrap();
        assert_eq!(docs.len(), 1, "rebuilt from git");
        assert_eq!(docs[0].body, SOP);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_agent_s_edit_waits_for_a_person() {
        let (dir, file) = temp();
        let store = Store::open(&file).unwrap();
        let ws = store.create_workspace("W").unwrap();
        let c = store.channels(&ws.id).unwrap()[0].clone();
        let doc = store.create_doc(&c.id, DEMO_NAME, "Intake SOP").unwrap();
        store.save_doc(&c.id, DEMO_NAME, &doc.path, SOP).unwrap();

        let edited = SOP.replace("before 5pm", "before 4pm");
        let p = store.propose_doc(&c.id, "intake", Some(&doc.path), None, &edited, "The office closes at 4:30 now").unwrap();
        assert_eq!(store.doc(&c.id, &doc.path).unwrap().body, SOP, "nothing changes until a person approves");
        assert!(!p.stale && !p.new_doc);
        let changed: Vec<(&str, &str)> = p.diff.iter().filter(|l| l.op != "same").map(|l| (l.op, l.text.as_str())).collect();
        assert_eq!(changed, [("del", "Scan them before 5pm."), ("add", "Scan them before 4pm.")]);

        store.approve_proposal(&c.id, DEMO_NAME, &p.id).unwrap();
        assert_eq!(store.doc(&c.id, &doc.path).unwrap().body, edited);
        assert!(store.proposals(&c.id).unwrap().is_empty());
        assert!(store.approve_proposal(&c.id, DEMO_NAME, &p.id).unwrap_err().contains("already"));
        let log: Vec<String> = store.doc_history(&c.id, &doc.path).unwrap().into_iter().map(|h| format!("{}: {}", h.author, h.message)).collect();
        assert_eq!(log[0], "Demo: Approve intake's change to Intake SOP: The office closes at 4:30 now");

        // A proposal on a doc that changed since can't be approved.
        let p = store.propose_doc(&c.id, "intake", Some(&doc.path), None, &format!("{edited}\nAsk for ID.\n"), "Ask for ID").unwrap();
        store.save_doc(&c.id, DEMO_NAME, &doc.path, &format!("{edited}\nBring a pen.\n")).unwrap();
        assert!(store.proposals(&c.id).unwrap()[0].stale);
        assert!(store.approve_proposal(&c.id, DEMO_NAME, &p.id).unwrap_err().contains("changed after"));
        store.reject_proposal(&c.id, DEMO_NAME, &p.id).unwrap();
        assert!(store.proposals(&c.id).unwrap().is_empty());
        assert!(store.doc(&c.id, &doc.path).unwrap().body.contains("Bring a pen") && !store.doc(&c.id, &doc.path).unwrap().body.contains("Ask for ID"));
        // What was rejected stays in the history.
        let repo = gix::open(records::repo_path(&dir, &c.id)).unwrap();
        assert!(repo.find_reference(format!("refs/gild/rejected/{}", p.id).as_str()).is_ok());

        // A new doc can be proposed too, and appears only when approved.
        let p = store.propose_doc(&c.id, "intake", None, Some("Closing"), "# Closing\n\nLock the door.\n", "A closing checklist").unwrap();
        assert!(p.new_doc && store.docs(&c.id).unwrap().len() == 1);
        store.approve_proposal(&c.id, DEMO_NAME, &p.id).unwrap();
        assert_eq!(store.doc(&c.id, "docs/closing.md").unwrap().title, "Closing");
        assert!(store.propose_doc(&c.id, "intake", None, None, "x", "y").is_err());
        assert!(store.propose_doc(&c.id, "intake", Some("docs/closing.md"), None, "x", " ").unwrap_err().contains("summary"));

        // Each step is said in the channel, so people see it happen.
        let said: Vec<String> = store.messages(&c.id).unwrap().into_iter().filter(|m| m.kind == "system").map(|m| m.body).collect();
        assert_eq!(said[0], "intake proposed a change to Intake SOP: The office closes at 4:30 now. It waits for approval in Docs.");
        assert_eq!(said[1], "Demo approved intake's change to Intake SOP.");
        assert_eq!(said[3], "Demo rejected intake's change to Intake SOP.");
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn search_finds_the_section_that_answers() {
        let (dir, file) = temp();
        let store = Store::open(&file).unwrap();
        let ws = store.create_workspace("W").unwrap();
        let c = store.channels(&ws.id).unwrap()[0].clone();
        let other = store.create_channel(&ws.id, "payroll", "").unwrap();
        let doc = store.create_doc(&c.id, DEMO_NAME, "Intake SOP").unwrap();
        store.save_doc(&c.id, DEMO_NAME, &doc.path, SOP).unwrap();
        let pay = store.create_doc(&other.id, DEMO_NAME, "Pay").unwrap();
        store.save_doc(&other.id, DEMO_NAME, &pay.path, "# Pay\n\nIntake forms for payroll go to the accountant.\n").unwrap();

        let hits = store.search_docs(std::slice::from_ref(&c.id), "Where do the intake forms go?").unwrap();
        assert_eq!((hits[0].title.as_str(), hits[0].section.as_str()), ("Intake SOP", "Step 2: Forms"));
        assert_eq!(hits[0].excerpt, "Signed intake forms go in the blue folder at the front desk.");
        assert!(hits.iter().all(|h| h.channel_id == c.id), "only the channels searched");
        assert!(store.search_docs(std::slice::from_ref(&c.id), "where is it?").is_err());
        assert!(store.search_docs(std::slice::from_ref(&c.id), "zebra").unwrap().is_empty());
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mentions_reach_only_agents_the_channel_is_shared_with() {
        assert_eq!(mentioned_names("@Intake can you check? cc @front-desk, mail sam@intake.org"), ["intake", "front-desk"]);
        assert!(mentioned_names("no one here, just an @ sign").is_empty());

        let (dir, file) = temp();
        let store = Store::open(&file).unwrap();
        let ws = store.create_workspace("W").unwrap();
        let c = store.channels(&ws.id).unwrap()[0].clone();
        let (intake, _) = store.create_agent("intake").unwrap();
        let (payroll, _) = store.create_agent("payroll").unwrap();
        store.set_access(&c.id, DEMO_NAME, &intake.id, Some(Access::Read)).unwrap();

        store.post(&c.id, DEMO_NAME, "person", "@intake where do the forms go? @payroll too").unwrap();
        let waiting = store.pending_mentions(&intake.id).unwrap();
        assert_eq!((waiting.len(), waiting[0].from.as_str(), waiting[0].channel.as_str()), (1, "Demo", "general"));
        assert!(store.pending_mentions(&payroll.id).unwrap().is_empty(), "payroll can't see the channel");
        let said = store.messages(&c.id).unwrap().last().unwrap().body.clone();
        assert_eq!(said, "payroll can't see this channel, so it wasn't told. Share the channel with payroll from Access.");

        // An agent mentioning itself doesn't wake itself.
        store.post(&c.id, "intake", "agent", "I'm @intake, on it.").unwrap();
        assert_eq!(store.pending_mentions(&intake.id).unwrap().len(), 1);

        // Mentions are handed out once.
        assert_eq!(store.take_mentions(&intake).unwrap().len(), 1);
        assert!(store.take_mentions(&intake).unwrap().is_empty());

        // Only agents with a command and something waiting are woken.
        store.post(&c.id, DEMO_NAME, "person", "@intake one more").unwrap();
        assert!(store.agents_to_wake().unwrap().is_empty(), "no command yet");
        store.set_wake(&intake.id, "  claude -p  ").unwrap();
        let wake: Vec<(String, String)> = store.agents_to_wake().unwrap().into_iter().map(|(a, c)| (a.name, c)).collect();
        assert_eq!(wake, [("intake".to_string(), "claude -p".to_string())]);
        assert_eq!(store.agent_infos().unwrap().iter().find(|a| a.agent.id == intake.id).unwrap().wake.as_deref(), Some("claude -p"));
        store.set_wake(&intake.id, "").unwrap();
        assert!(store.agent_infos().unwrap().iter().all(|a| a.wake.is_none()));

        // Unsharing the channel drops what was waiting there.
        store.set_access(&c.id, DEMO_NAME, &intake.id, None).unwrap();
        assert!(store.take_mentions(&intake).unwrap().is_empty());
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn channel_names() {
        assert_eq!(channel_name("Student Intake").as_deref(), Some("student-intake"));
        assert_eq!(channel_name("  #Agent  fleet!! ").as_deref(), Some("agent-fleet"));
        assert_eq!(channel_name("café_menu").as_deref(), Some("café-menu"));
        assert_eq!(channel_name("---"), None);
        assert_eq!(channel_name(&"a".repeat(80)).map(|s| s.len()), Some(40));
    }

    #[test]
    fn agents_see_only_what_they_are_given() {
        let (dir, file) = temp();
        let store = Store::open(&file).unwrap();
        let ws = store.create_workspace("W").unwrap();
        let general = store.channels(&ws.id).unwrap()[0].clone();
        let secret = store.create_channel(&ws.id, "payroll", "").unwrap();
        let (agent, token) = store.create_agent("Intake").unwrap();
        assert_eq!(agent.name, "intake");
        assert!(token.starts_with("gild_") && token.len() == 5 + 64);
        assert_eq!(store.agent_for_token(&token).unwrap().unwrap().id, agent.id);
        assert!(store.agent_for_token("gild_wrong").unwrap().is_none());
        assert!(store.create_agent("intake").is_err(), "names are unique");

        // Nothing is shared until someone shares it.
        assert!(store.agent_channels(&agent).unwrap().is_empty());
        assert!(store.require(&agent, &general.id, Access::Read).is_err());

        store.set_access(&general.id, DEMO_NAME, &agent.id, Some(Access::Read)).unwrap();
        assert!(store.require(&agent, &general.id, Access::Read).is_ok());
        assert!(store.require(&agent, &general.id, Access::Write).unwrap_err().contains("not change"));
        assert!(store.require(&agent, &secret.id, Access::Read).is_err());
        let seen: Vec<String> = store.agent_channels(&agent).unwrap().into_iter().map(|c| c.channel.name).collect();
        assert_eq!(seen, ["general"]);

        // Grants are commits, and they survive losing the index.
        store.set_access(&general.id, DEMO_NAME, &agent.id, Some(Access::Write)).unwrap();
        let log: Vec<String> = records::history(&dir, &general.id, records::ACL_PATH).unwrap().into_iter().map(|c| c.message).collect();
        assert_eq!(log, ["Let intake read and change this channel", "Let intake read this channel"]);
        store.run("DELETE FROM grants", &[]).unwrap();
        store.run("DELETE FROM indexed", &[]).unwrap();
        drop(store);
        let store = Store::open(&file).unwrap();
        assert_eq!(store.access(&agent.id, &general.id).unwrap(), Some(Access::Write));

        store.set_access(&general.id, DEMO_NAME, &agent.id, None).unwrap();
        assert!(store.require(&agent, &general.id, Access::Read).is_err());
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
