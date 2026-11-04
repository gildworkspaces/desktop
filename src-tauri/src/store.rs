//! The workspace's local store: workspaces, channels and messages in one
//! SQLite file in the app's data folder, written with Turso's Rust engine
//! (`turso_core`), the same engine Zega+SQLite uses. No C.
//!
//! Every read and write in the app goes through these functions; the window
//! never touches the file.

use std::num::NonZero;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use turso_core::{Connection, Database, PlatformIO, SqliteDialect, Value, IO};

/// Bumped when a table's columns or meaning change.
const FORMAT: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS gild_meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS workspaces (id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS channels (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, name TEXT NOT NULL, topic TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS messages (id TEXT PRIMARY KEY, channel_id TEXT NOT NULL, author TEXT NOT NULL, kind TEXT NOT NULL, body TEXT NOT NULL, created_at INTEGER NOT NULL);
";

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

pub struct Store {
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
    pub fn open(path: &Path) -> Result<Store, String> {
        let path = path.to_str().ok_or("the data folder's path is not valid UTF-8")?;
        let io: Arc<dyn IO> = Arc::new(PlatformIO::new().map_err(|e| err("starting SQLite I/O", e))?);
        let db = Database::open_file(io, path, Arc::new(SqliteDialect)).map_err(|e| err("opening the workspace database", e))?;
        let conn = db.connect().map_err(|e| err("connecting to the workspace database", e))?;
        conn.execute("PRAGMA journal_mode = wal")
            .and_then(|()| conn.execute("PRAGMA synchronous = FULL"))
            .and_then(|()| conn.execute(SCHEMA))
            .map_err(|e| err("preparing the workspace database", e))?;
        let store = Store { conn, _db: db };
        match store.query("SELECT value FROM gild_meta WHERE key = 'format'", &[])?.first() {
            None => store.run("INSERT INTO gild_meta (key, value) VALUES ('format', ?1)", &[Value::from_i64(FORMAT)])?,
            Some(row) if int(&row[0]) == FORMAT => {}
            Some(row) => return Err(format!("this database is format {}, and this gild reads format {FORMAT}", int(&row[0]))),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("gild-store-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("gild.db");
        (dir, file)
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
