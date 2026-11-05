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
//!   docs/<name>.md              the channel's docs, plain markdown
//!
//! An agent's edit to a doc is a branch, `proposals/<id>`, until a person
//! approves it (a merge commit) or rejects it (the branch moves to
//! `refs/gild/rejected/<id>`, so what was proposed stays in the history).
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

/// A markdown doc: `docs/<name>.md`. Its title is its first `# ` heading.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Doc {
    pub path: String,
    pub title: String,
    pub body: String,
}

impl Doc {
    pub fn new(path: &str, body: String) -> Doc {
        let title = body
            .lines()
            .find_map(|l| l.strip_prefix("# ").map(str::trim).filter(|t| !t.is_empty()))
            .map(str::to_string)
            .unwrap_or_else(|| path.trim_start_matches("docs/").trim_end_matches(".md").to_string());
        Doc { path: path.to_string(), title, body }
    }
}

/// An agent's proposed change to a doc, waiting for a person: a branch
/// `proposals/<id>` whose one commit sits on the channel's history.
pub struct RawProposal {
    pub id: String,
    pub author: String,
    pub at: i64,
    pub message: String,
    pub path: String,
    /// The doc when the proposal was made (None: a new doc).
    pub before: Option<String>,
    pub after: String,
    /// The doc changed after the proposal was made, so it can't be approved
    /// as it is.
    pub stale: bool,
}

/// One line of a diff: `same`, `add` or `del`.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct DiffLine {
    pub op: &'static str,
    pub text: String,
}

/// Line by line, what changed from `before` to `after`.
pub fn diff(before: &str, after: &str) -> Vec<DiffLine> {
    similar::TextDiff::from_lines(before, after)
        .iter_all_changes()
        .map(|c| DiffLine {
            op: match c.tag() {
                similar::ChangeTag::Equal => "same",
                similar::ChangeTag::Insert => "add",
                similar::ChangeTag::Delete => "del",
            },
            text: c.value().trim_end_matches('\n').to_string(),
        })
        .collect()
}

pub const DOCS_DIR: &str = "docs";
const PROPOSALS: &str = "refs/heads/proposals/";
const REJECTED: &str = "refs/gild/rejected/";

/// What a channel's repository holds at one commit.
pub struct Snapshot {
    pub commit: Option<String>,
    pub acl: Acl,
    pub view: Option<View>,
    pub records: Vec<Record>,
    pub docs: Vec<Doc>,
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
    let id = commit_on(&repo, "HEAD", parent, parent.into_iter().collect(), files, author, message)?;
    Ok(id.to_string())
}

/// One commit: `base`'s tree with `files` written over it, with `parents`,
/// moving `reference` to it.
fn commit_on(
    repo: &gix::Repository,
    reference: &str,
    base: Option<ObjectId>,
    parents: Vec<ObjectId>,
    files: &[(String, Vec<u8>)],
    author: &str,
    message: &str,
) -> Result<ObjectId, String> {
    let base = match base {
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
        .commit_as(who, who, reference, message, tree, parents)
        .map_err(|e| err("saving the change", e))?;
    Ok(id.detach())
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
    let empty = || Snapshot { commit: None, acl: Acl::default(), view: None, records: vec![], docs: vec![] };
    let path = repo_path(data, channel_id);
    if !path.exists() {
        return Ok(empty());
    }
    let repo = gix::open(&path).map_err(|e| err("opening the channel's history", e))?;
    let Some(id) = head(&repo) else {
        return Ok(empty());
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
    let mut docs = Vec::new();
    for (path, blob) in doc_blobs(&repo, &tree)? {
        let bytes = repo.find_object(blob).map_err(|e| err("reading a doc", e))?.data.clone();
        docs.push(Doc::new(&path, String::from_utf8_lossy(&bytes).into_owned()));
    }
    Ok(Snapshot { commit: Some(id.to_string()), acl, view, records, docs })
}

/// Every `docs/*.md` in a tree: path → blob.
fn doc_blobs(repo: &gix::Repository, tree: &gix::Tree<'_>) -> Result<std::collections::BTreeMap<String, ObjectId>, String> {
    let mut out = std::collections::BTreeMap::new();
    let Some(dir) = tree.lookup_entry_by_path(DOCS_DIR).map_err(|e| err("reading the docs", e))? else {
        return Ok(out);
    };
    let dir = repo
        .find_object(dir.object_id())
        .and_then(|o| o.try_into_tree())
        .map_err(|e| err("reading the docs", e))?;
    for entry in dir.iter() {
        let entry = entry.map_err(|e| err("reading the docs", e))?;
        let name = entry.filename().to_string();
        if entry.mode().is_blob() && name.ends_with(".md") {
            out.insert(format!("{DOCS_DIR}/{name}"), entry.object_id());
        }
    }
    Ok(out)
}

/// A file in a channel's repository, as the view lists it.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
}

/// Every file at the repository's current commit, by path, except gild's own
/// `.gild/` folder (the table's records, access, the view's settings).
pub fn files(data: &Path, channel_id: &str) -> Result<Vec<FileEntry>, String> {
    let path = repo_path(data, channel_id);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let repo = gix::open(&path).map_err(|e| err("opening the channel's history", e))?;
    let Some(id) = head(&repo) else { return Ok(Vec::new()) };
    let tree = tree_of(&repo, id)?;
    let mut out = Vec::new();
    walk(&repo, &tree, "", &mut out)?;
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn walk(repo: &gix::Repository, tree: &gix::Tree<'_>, prefix: &str, out: &mut Vec<FileEntry>) -> Result<(), String> {
    for entry in tree.iter() {
        let entry = entry.map_err(|e| err("reading the files", e))?;
        let name = entry.filename().to_string();
        if prefix.is_empty() && name == ".gild" {
            continue;
        }
        let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        let object = repo.find_object(entry.object_id()).map_err(|e| err("reading the files", e))?;
        if entry.mode().is_tree() {
            let sub = object.try_into_tree().map_err(|e| err("reading the files", e))?;
            walk(repo, &sub, &path, out)?;
        } else if entry.mode().is_blob() {
            out.push(FileEntry { path, size: object.data.len() as u64 });
        }
    }
    Ok(())
}

/// A file's bytes at the repository's current commit (None: no such file).
pub fn read_file(data: &Path, channel_id: &str, path: &str) -> Result<Option<Vec<u8>>, String> {
    let repo_dir = repo_path(data, channel_id);
    if !repo_dir.exists() {
        return Ok(None);
    }
    let repo = gix::open(&repo_dir).map_err(|e| err("opening the channel's history", e))?;
    let Some(id) = head(&repo) else { return Ok(None) };
    let tree = tree_of(&repo, id)?;
    let bytes = blob_text(&repo, &tree, path)?;
    Ok(bytes)
}

fn tree_of(repo: &gix::Repository, commit: ObjectId) -> Result<gix::Tree<'_>, String> {
    repo.find_commit(commit).and_then(|c| c.tree()).map_err(|e| err("reading history", e))
}

fn blob_at(repo: &gix::Repository, commit: Option<ObjectId>, path: &str) -> Result<Option<ObjectId>, String> {
    let Some(commit) = commit else { return Ok(None) };
    let tree = tree_of(repo, commit)?;
    Ok(tree.lookup_entry_by_path(path).map_err(|e| err("reading history", e))?.map(|e| e.object_id()))
}

fn text_of(repo: &gix::Repository, blob: Option<ObjectId>) -> Result<Option<String>, String> {
    let Some(blob) = blob else { return Ok(None) };
    let object = repo.find_object(blob).map_err(|e| err("reading a doc", e))?;
    Ok(Some(String::from_utf8_lossy(&object.data).into_owned()))
}

/// Put `body` at `path` on a new branch `proposals/<id>`, on top of the
/// channel's current history. The channel's docs don't change.
pub fn propose(data: &Path, channel_id: &str, id: &str, author: &str, message: &str, path: &str, body: &[u8]) -> Result<(), String> {
    let repo = open_or_init(&repo_path(data, channel_id))?;
    let base = head(&repo).ok_or("This channel has no history to propose a change to.")?;
    let files = [(path.to_string(), body.to_vec())];
    commit_on(&repo, &format!("{PROPOSALS}{id}"), Some(base), vec![base], &files, author, message)?;
    Ok(())
}

/// The proposal's commit, the commit it was made on, and the doc it changes.
fn proposal_parts(repo: &gix::Repository, id: &str) -> Result<(ObjectId, ObjectId, String), String> {
    let mut reference = repo
        .find_reference(format!("{PROPOSALS}{id}").as_str())
        .map_err(|_| "That proposal was already approved or rejected.".to_string())?;
    let commit = reference.peel_to_id().map_err(|e| err("reading the proposal", e))?.detach();
    let parent = repo
        .find_commit(commit)
        .map_err(|e| err("reading the proposal", e))?
        .parent_ids()
        .next()
        .ok_or("The proposal has no base.")?
        .detach();
    let (before, after) = (doc_blobs(repo, &tree_of(repo, parent)?)?, doc_blobs(repo, &tree_of(repo, commit)?)?);
    let path = after
        .iter()
        .find(|(p, b)| before.get(*p) != Some(b))
        .map(|(p, _)| p.clone())
        .ok_or("The proposal changes no doc.")?;
    Ok((commit, parent, path))
}

/// Every open proposal, oldest first.
pub fn proposals(data: &Path, channel_id: &str) -> Result<Vec<RawProposal>, String> {
    let path = repo_path(data, channel_id);
    if !path.exists() {
        return Ok(vec![]);
    }
    let repo = gix::open(&path).map_err(|e| err("opening the channel's history", e))?;
    let current = head(&repo);
    let mut ids = Vec::new();
    let platform = repo.references().map_err(|e| err("reading proposals", e))?;
    for r in platform.local_branches().map_err(|e| err("reading proposals", e))? {
        let r = r.map_err(|e| err("reading proposals", e))?;
        if let Some(id) = r.name().as_bstr().to_string().strip_prefix(PROPOSALS) {
            ids.push(id.to_string());
        }
    }
    let mut out = Vec::new();
    for id in ids {
        let (commit, parent, path) = proposal_parts(&repo, &id)?;
        let c = repo.find_commit(commit).map_err(|e| err("reading the proposal", e))?;
        let author = c.author().map_err(|e| err("reading the proposal", e))?;
        let (was, now) = (blob_at(&repo, Some(parent), &path)?, blob_at(&repo, current, &path)?);
        out.push(RawProposal {
            id,
            author: author.name.to_string(),
            at: author.time().map(|t| t.seconds).unwrap_or(0) * 1000,
            message: c.message_raw_sloppy().to_string().trim().to_string(),
            before: text_of(&repo, was)?,
            after: text_of(&repo, blob_at(&repo, Some(commit), &path)?)?.unwrap_or_default(),
            stale: was != now,
            path,
        });
    }
    out.sort_by(|a, b| (a.at, &a.id).cmp(&(b.at, &b.id)));
    Ok(out)
}

/// Take a proposal into the channel's history as a merge commit by
/// `author`: the doc becomes what the agent proposed, and both the agent's
/// commit and the approval stay in the history.
pub fn approve(data: &Path, channel_id: &str, id: &str, author: &str, message: &str) -> Result<(), String> {
    let repo = gix::open(repo_path(data, channel_id)).map_err(|e| err("opening the channel's history", e))?;
    let (commit, parent, path) = proposal_parts(&repo, id)?;
    let current = head(&repo).ok_or("This channel has no history.")?;
    if blob_at(&repo, Some(parent), &path)? != blob_at(&repo, Some(current), &path)? {
        return Err("The doc changed after this was proposed. Reject it and ask for a fresh proposal.".into());
    }
    let body = text_of(&repo, blob_at(&repo, Some(commit), &path)?)?.unwrap_or_default();
    commit_on(&repo, "HEAD", Some(current), vec![current, commit], &[(path, body.into_bytes())], author, message)?;
    delete_proposal(&repo, id)
}

/// Close a proposal without taking it. Its commit stays reachable from
/// `refs/gild/rejected/<id>`, so the history keeps what was proposed.
pub fn reject(data: &Path, channel_id: &str, id: &str) -> Result<(), String> {
    let repo = gix::open(repo_path(data, channel_id)).map_err(|e| err("opening the channel's history", e))?;
    let (commit, _, _) = proposal_parts(&repo, id)?;
    repo.reference(format!("{REJECTED}{id}").as_str(), commit, gix::refs::transaction::PreviousValue::MustNotExist, "rejected")
        .map_err(|e| err("keeping the rejected proposal", e))?;
    delete_proposal(&repo, id)
}

fn delete_proposal(repo: &gix::Repository, id: &str) -> Result<(), String> {
    repo.find_reference(format!("{PROPOSALS}{id}").as_str())
        .map_err(|e| err("closing the proposal", e))?
        .delete()
        .map_err(|e| err("closing the proposal", e))
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
