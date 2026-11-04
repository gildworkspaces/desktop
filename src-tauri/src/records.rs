//! A channel's data: its view and its records, kept in a git repository per
//! channel (`<data>/channels/<channel id>.git`, bare). Git is the truth; the
//! SQLite tables in `store.rs` are an index rebuilt from it.
//!
//! In the repository:
//!   .gild/acl.json              who besides the workspace's people may use
//!                               this channel: agent id → "read" | "write"
//!   .gild/view.json             the channel's view: columns and statuses
//!   .gild/records/<id>.json     one file per record, so two writers never
//!                               touch the same file for different records
//!
//! Every change is one commit, written by the person who made it. Pure Rust
//! (gitoxide): no C and no `git` binary needed.

use std::path::{Path, PathBuf};

use gix::bstr::BString;
use gix::objs::tree::EntryKind;
use gix::ObjectId;
use serde::{Deserialize, Serialize};

pub const VIEW_PATH: &str = ".gild/view.json";
pub const ACL_PATH: &str = ".gild/acl.json";

/// What an agent may do in a channel. No entry means nothing: an agent sees
/// only the channels it was given.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Read,
    Write,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Acl {
    pub agents: std::collections::BTreeMap<String, Access>,
}
pub const RECORDS_DIR: &str = ".gild/records";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct View {
    /// `table` (shown as a table or a board); more kinds come later.
    pub kind: String,
    /// The columns after the title, in order.
    pub fields: Vec<String>,
    /// The board's columns, in order. A record's status is one of these.
    pub statuses: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Record {
    pub id: String,
    pub title: String,
    pub status: String,
    /// One value per view field, by name.
    pub fields: std::collections::BTreeMap<String, String>,
    pub created_at: i64,
}

/// One commit that changed something, newest first.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Change {
    pub commit: String,
    pub author: String,
    pub at: i64,
    pub message: String,
}

/// What a channel's repository holds at one commit.
pub struct Snapshot {
    pub commit: Option<String>,
    pub acl: Acl,
    pub view: Option<View>,
    pub records: Vec<Record>,
}

fn err(context: &str, e: impl std::fmt::Display) -> String {
    format!("{context}: {e}")
}

pub fn repo_path(data: &Path, channel_id: &str) -> PathBuf {
    data.join("channels").join(format!("{channel_id}.git"))
}

pub fn record_path(id: &str) -> String {
    format!("{RECORDS_DIR}/{id}.json")
}

fn open_or_init(path: &Path) -> Result<gix::Repository, String> {
    if path.exists() {
        gix::open(path).map_err(|e| err("opening the channel's history", e))
    } else {
        std::fs::create_dir_all(path).map_err(|e| err("creating the channel's history", e))?;
        gix::init_bare(path).map_err(|e| err("creating the channel's history", e))
    }
}

fn head(repo: &gix::Repository) -> Option<ObjectId> {
    repo.head_id().ok().map(|id| id.detach())
}

/// The commit a channel's repository is at, if it has one.
pub fn head_commit(data: &Path, channel_id: &str) -> Result<Option<String>, String> {
    let path = repo_path(data, channel_id);
    if !path.exists() {
        return Ok(None);
    }
    let repo = gix::open(&path).map_err(|e| err("opening the channel's history", e))?;
    Ok(head(&repo).map(|id| id.to_string()))
}

/// Write `files` (path → bytes) as one commit by `author`, and return its id.
pub fn commit(data: &Path, channel_id: &str, author: &str, message: &str, files: &[(String, Vec<u8>)]) -> Result<String, String> {
    let repo = open_or_init(&repo_path(data, channel_id))?;
    let parent = head(&repo);
    let base = match parent {
        Some(id) => repo
            .find_commit(id)
            .and_then(|c| c.tree_id())
            .map_err(|e| err("reading the channel's last change", e))?
            .detach(),
        None => repo.empty_tree().id,
    };
    let mut editor = repo.edit_tree(base).map_err(|e| err("preparing the change", e))?;
    for (path, bytes) in files {
        let blob = repo.write_blob(bytes).map_err(|e| err("writing a file", e))?;
        editor.upsert(path.as_str(), EntryKind::Blob, blob).map_err(|e| err("adding a file", e))?;
    }
    let tree = editor.write().map_err(|e| err("writing the change", e))?;
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let signature = gix::actor::Signature {
        name: BString::from(author),
        email: BString::from(format!("{}@gild.local", author.to_lowercase().replace(' ', "."))),
        time: gix::date::Time { seconds, offset: 0 },
    };
    let mut time = gix::date::parse::TimeBuf::default();
    let who = signature.to_ref(&mut time);
    let id = repo
        .commit_as(who, who, "HEAD", message, tree, parent)
        .map_err(|e| err("saving the change", e))?;
    Ok(id.to_string())
}

fn blob_text(repo: &gix::Repository, tree: &gix::Tree<'_>, path: &str) -> Result<Option<Vec<u8>>, String> {
    let Some(entry) = tree.lookup_entry_by_path(path).map_err(|e| err("reading a file", e))? else {
        return Ok(None);
    };
    let object = repo.find_object(entry.object_id()).map_err(|e| err("reading a file", e))?;
    Ok(Some(object.data.clone()))
}

/// Everything at the repository's current commit: the source the index is
/// built from.
pub fn snapshot(data: &Path, channel_id: &str) -> Result<Snapshot, String> {
    let path = repo_path(data, channel_id);
    if !path.exists() {
        return Ok(Snapshot { commit: None, acl: Acl::default(), view: None, records: vec![] });
    }
    let repo = gix::open(&path).map_err(|e| err("opening the channel's history", e))?;
    let Some(id) = head(&repo) else {
        return Ok(Snapshot { commit: None, acl: Acl::default(), view: None, records: vec![] });
    };
    let tree = repo
        .find_commit(id)
        .and_then(|c| c.tree())
        .map_err(|e| err("reading the channel's last change", e))?;
    let acl = match blob_text(&repo, &tree, ACL_PATH)? {
        Some(bytes) => serde_json::from_slice::<Acl>(&bytes).map_err(|e| err("reading .gild/acl.json", e))?,
        None => Acl::default(),
    };
    let view = match blob_text(&repo, &tree, VIEW_PATH)? {
        Some(bytes) => Some(serde_json::from_slice::<View>(&bytes).map_err(|e| err("reading .gild/view.json", e))?),
        None => None,
    };
    let mut records = Vec::new();
    if let Some(dir) = tree.lookup_entry_by_path(RECORDS_DIR).map_err(|e| err("reading the records", e))? {
        let dir = repo
            .find_object(dir.object_id())
            .and_then(|o| o.try_into_tree())
            .map_err(|e| err("reading the records", e))?;
        for entry in dir.iter() {
            let entry = entry.map_err(|e| err("reading the records", e))?;
            let name = entry.filename().to_string();
            let bytes = repo.find_object(entry.object_id()).map_err(|e| err("reading a record", e))?.data.clone();
            let record: Record = serde_json::from_slice(&bytes).map_err(|e| err(&format!("reading .gild/records/{name}"), e))?;
            records.push(record);
        }
    }
    records.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
    Ok(Snapshot { commit: Some(id.to_string()), acl, view, records })
}

/// The commits that changed `path`, newest first, following the first parent
/// (a channel's history is a line: one writer).
pub fn history(data: &Path, channel_id: &str, path: &str) -> Result<Vec<Change>, String> {
    let repo_dir = repo_path(data, channel_id);
    if !repo_dir.exists() {
        return Ok(vec![]);
    }
    let repo = gix::open(&repo_dir).map_err(|e| err("opening the channel's history", e))?;
    let blob_at = |id: ObjectId| -> Result<Option<ObjectId>, String> {
        let tree = repo.find_commit(id).and_then(|c| c.tree()).map_err(|e| err("reading history", e))?;
        Ok(tree.lookup_entry_by_path(path).map_err(|e| err("reading history", e))?.map(|e| e.object_id()))
    };
    let mut out = Vec::new();
    let mut next = head(&repo);
    while let Some(id) = next {
        let commit = repo.find_commit(id).map_err(|e| err("reading history", e))?;
        let parent = commit.parent_ids().next().map(|p| p.detach());
        let here = blob_at(id)?;
        let before = match parent {
            Some(p) => blob_at(p)?,
            None => None,
        };
        if here != before {
            let author = commit.author().map_err(|e| err("reading history", e))?;
            let at = author.time().map(|t| t.seconds).unwrap_or(0) * 1000;
            out.push(Change {
                commit: id.to_string(),
                author: author.name.to_string(),
                at,
                message: commit.message_raw_sloppy().to_string().trim().to_string(),
            });
        }
        next = parent;
    }
    Ok(out)
}
