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

use serde::Serialize;
use turso_core::{Connection, Database, PlatformIO, SqliteDialect, Value, IO};

use crate::records::{self, Change, Record, View};

/// Bumped when a table's columns or meaning change.
const FORMAT: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS gild_meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS workspaces (id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS channels (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, name TEXT NOT NULL, topic TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS messages (id TEXT PRIMARY KEY, channel_id TEXT NOT NULL, author TEXT NOT NULL, kind TEXT NOT NULL, body TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS views (channel_id TEXT PRIMARY KEY, config TEXT NOT NULL, indexed_commit TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS records (channel_id TEXT NOT NULL, id TEXT NOT NULL, title TEXT NOT NULL, status TEXT NOT NULL, fields TEXT NOT NULL, created_at INTEGER NOT NULL, PRIMARY KEY (channel_id, id));
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

#[derive(Serialize, Clone, Debug, PartialEq)]
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
}

fn err(context: &str, e: impl std::fmt::Display) -> String {
    format!("{context}: {e}")
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
        let store = Store { data: data.to_path_buf(), conn, _db: db };
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
                .query("SELECT indexed_commit FROM views WHERE channel_id = ?1", &[Value::from_text(channel.clone())])?
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

    #[test]
    fn channel_names() {
        assert_eq!(channel_name("Student Intake").as_deref(), Some("student-intake"));
        assert_eq!(channel_name("  #Agent  fleet!! ").as_deref(), Some("agent-fleet"));
        assert_eq!(channel_name("café_menu").as_deref(), Some("café-menu"));
        assert_eq!(channel_name("---"), None);
        assert_eq!(channel_name(&"a".repeat(80)).map(|s| s.len()), Some(40));
    }
}
