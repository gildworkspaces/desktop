//! A channel's data: its view and its records, kept in a git repository per
//! channel, an ordinary one with its files checked out, in the workspace's
//! folder (see `store/folders.rs`). Git is the truth; the SQLite tables in
//! `store.rs` are an index rebuilt from it.
//!
//! In the repository:
//!   .gild/acl.json              who besides the workspace's people may use
//!                               this channel: agent id → "read" | "write",
//!                               and the org and teams it's open to
//!   .gild/view.json             the channel's view: columns and statuses
//!   .gild/records/<id>.json     one file per record, so two writers never
//!                               touch the same file for different records
//!   docs/<name>.md              the channel's docs, plain markdown
//!
//! An agent's edit to a doc is a branch, `proposals/<id>`, until a person
//! approves it (a merge commit) or rejects it (the branch moves to
//! `refs/gild/rejected/<id>`, so what was proposed stays in the history).
//!
//! Every change is one commit, written by the person who made it, and the
//! files on disk and git's index match that commit after every write. Files
//! changed outside gild are committed as the person (`check_outside`). Pure
//! Rust (gitoxide): no C and no `git` binary needed.

use std::collections::{BTreeMap, HashMap};
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

/// Who may be in a channel: named agents, and, through the workspace's org,
/// everyone in it (`org`) or whole teams (`teams`: a seat and every seat
/// under it). `levels` sets read or change for a member who is in through
/// the org or a team (change when unset). Fields left at their default
/// aren't written, so a channel with only named agents keeps the file it had.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Acl {
    pub agents: std::collections::BTreeMap<String, Access>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub org: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub teams: Vec<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub levels: std::collections::BTreeMap<String, Access>,
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
/// The workspace file at the root of a workspace's main repository.
pub const MANIFEST: &str = "gild.json";
const HIDDEN_MANIFEST: &str = MANIFEST;
/// Where the other channels' repositories are, in the main repository's
/// folder (which ignores it).
pub const CHANNELS_DIR: &str = "channels";
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

pub fn record_path(id: &str) -> String {
    format!("{RECORDS_DIR}/{id}.json")
}

/// Files gild never takes into a channel's history: what file managers
/// leave in every folder they show. Kept in each repository's
/// `.git/info/exclude`, so git ignores them too without a commit.
const JUNK: &str = ".DS_Store\nThumbs.db\ndesktop.ini\n";

/// Whether `dir` holds a repository (with files, or bare).
pub fn is_repo(dir: &Path) -> bool {
    dir.join(".git").exists() || (dir.join("HEAD").is_file() && dir.join("objects").is_dir())
}

/// Settings in a repository's own config that gild won't work with. In
/// gitoxide 0.90, `core.worktree` moves where files are checked out and
/// written (gix/src/open/repository.rs), and `filter.<driver>.clean`,
/// `.smudge` and `.process` run commands through the shell during checkout
/// and status (gix-filter, picked by `.gitattributes`). `include` and
/// `includeIf` would pull settings in from other files. The rest run
/// programs in git itself (hooks, fsmonitor, ssh, diff and merge drivers):
/// refused too, so opening a folder in git after gild can't surprise anyone.
fn unsafe_setting(git_dir: &Path, has_folder: bool) -> Result<Option<String>, String> {
    for name in ["config", "config.worktree"] {
        let path = git_dir.join(name);
        if !path.exists() {
            continue;
        }
        let file = gix::config::File::from_path_no_includes(path, gix::config::Source::Local).map_err(|e| err("reading the repository's git settings", e))?;
        for s in file.sections() {
            let section = s.header().name().to_string().to_ascii_lowercase();
            let sub = s.header().subsection_name().map(|x| format!(".{x}")).unwrap_or_default();
            for key in s.value_names() {
                let key = key.to_ascii_lowercase();
                let bad = matches!(section.as_str(), "filter" | "include" | "includeif")
                    || (section == "core" && ["worktree", "fsmonitor", "hookspath", "sshcommand", "gitproxy", "askpass"].contains(&key.as_str()))
                    || (section == "diff" && (key == "external" || key == "textconv"))
                    || (section == "merge" && key == "driver")
                    // A channel's folder is its files: a "bare" one would
                    // take none of gild's writes and show none of yours.
                    || (has_folder && section == "core" && key == "bare" && s.values("bare").last().is_none_or(|v| ["true", "yes", "on", "1", ""].contains(&v.to_string().trim().to_ascii_lowercase().as_str())));
                if bad {
                    return Ok(Some(format!("{section}{sub}.{key}")));
                }
            }
        }
    }
    Ok(None)
}

/// Open a repository the safe way: never with a `.git` that is a link or a
/// pointer to another folder, never when its own config sets something in
/// `unsafe_setting`, and with no settings from outside it (no global or
/// system git config, no environment): a filter driver in someone's
/// `~/.gitconfig` doesn't run either. Its files must be in `dir`.
fn open(dir: &Path) -> Result<Option<gix::Repository>, String> {
    if !is_repo(dir) {
        return Ok(None);
    }
    let dot_git = dir.join(".git");
    // A channel's folder (with a .git), or one of the bare repositories
    // gild kept before 0.24, opened only to move it.
    let has_folder = std::fs::symlink_metadata(&dot_git).is_ok();
    let git_dir = if has_folder {
        if !std::fs::symlink_metadata(&dot_git).is_ok_and(|m| m.is_dir()) {
            return Err(format!("{}'s .git isn't a folder (it's a link, or points to another folder), so gild won't use it.", dir.display()));
        }
        dot_git
    } else {
        dir.to_path_buf()
    };
    if git_dir.join("commondir").exists() {
        return Err(format!("{} shares its history with another folder (a linked worktree), so gild won't use it.", dir.display()));
    }
    if let Some(key) = unsafe_setting(&git_dir, has_folder)? {
        return Err(format!(
            "{}'s git settings set {key}, which could make git write outside the folder, ignore it, or run a program. gild won't use it until that setting is removed from .git/config.",
            dir.display()
        ));
    }
    if has_folder {
        // Objects borrowed from another repository on this computer.
        for f in ["objects/info/alternates", "objects/info/http-alternates"] {
            if git_dir.join(f).exists() {
                return Err(format!("{} borrows history from another repository (.git/{f}), so gild won't use it. Delete that file to use the folder.", dir.display()));
            }
        }
        // gild never runs hooks, but the person's own git would. git's
        // `*.sample` files and the `docs.url` gitoxide's `init` writes are
        // never run.
        if let Ok(entries) = std::fs::read_dir(git_dir.join("hooks")) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if !name.ends_with(".sample") && name != "docs.url" {
                    return Err(format!("{} has a git hook, .git/hooks/{name}, which git would run. gild won't use the folder while it's there: delete it and try again.", dir.display()));
                }
            }
        }
    }
    let repo = gix::open_opts(dir, gix::open::Options::isolated()).map_err(|e| err("opening the channel's history", e))?;
    match repo.workdir() {
        Some(work) => {
            let same = matches!((work.canonicalize(), dir.canonicalize()), (Ok(a), Ok(b)) if a == b);
            if !same {
                return Err(format!("{}'s git settings put its files somewhere else, so gild won't use it.", dir.display()));
            }
        }
        None if has_folder => {
            return Err(format!("{}'s repository has no files (it's set up bare), so gild won't use it.", dir.display()));
        }
        None => {}
    }
    Ok(Some(repo))
}

/// Whether gild will use the repository in `dir` (see `open`): Ok if there
/// is none, or one that's safe.
pub fn check(dir: &Path) -> Result<(), String> {
    open(dir).map(|_| ())
}

/// Whether `git_dir` looks like a repository's history to copy (its HEAD and
/// objects): nothing in it is opened or trusted.
pub fn is_git_dir(git_dir: &Path) -> bool {
    let real = |p: &Path, dir: bool| std::fs::symlink_metadata(p).is_ok_and(|m| if dir { m.is_dir() } else { m.is_file() });
    real(git_dir, true) && real(&git_dir.join("HEAD"), false) && real(&git_dir.join("objects"), true)
}

/// Make `dir` a repository with its files checked out, if it isn't one.
pub fn init(dir: &Path) -> Result<(), String> {
    open_or_init(dir).map(|_| ())
}

fn open_or_init(dir: &Path) -> Result<gix::Repository, String> {
    if let Some(repo) = open(dir)? {
        return Ok(repo);
    }
    std::fs::create_dir_all(dir).map_err(|e| err("creating the channel's folder", e))?;
    gix::init(dir).map_err(|e| err("creating the channel's history", e))?;
    fresh_config(&dir.join(".git"))?;
    open_existing(dir)
}

/// A repository gild made, moved or copied gets a config of its own,
/// written from scratch: none of another repository's settings (remotes and
/// their passwords, credential helpers, a worktree elsewhere, filters) come
/// with a copy. The file system's details are probed, as `git init` does.
fn fresh_config(git_dir: &Path) -> Result<(), String> {
    let caps = gix::fs::Capabilities::probe(git_dir);
    let text = format!(
        "[core]\n\trepositoryformatversion = 0\n\tfilemode = {}\n\tlogallrefupdates = true\n\tignorecase = {}\n\tprecomposeunicode = {}\n",
        caps.executable_bit, caps.ignore_case, caps.precompose_unicode
    );
    std::fs::write(git_dir.join("config"), text).map_err(|e| err("setting up the channel's history", e))?;
    let _ = std::fs::remove_file(git_dir.join("config.worktree"));
    own_config(git_dir)
}

/// An existing repository: gild never makes one where one is missing,
/// except when a channel is created (`init`).
fn open_existing(dir: &Path) -> Result<gix::Repository, String> {
    open(dir)?.ok_or_else(|| format!("{} has no channel history. Put the channel's folder back.", dir.display()))
}

/// The file in `.git` that says which channel a repository is: written
/// before gild puts anything there, so a folder that is someone else's (or
/// another channel's) is never written over.
const CHANNEL_MARK: &str = "gild-channel";
/// The file in `.git` naming the branch gild commits to.
const BRANCH_FILE: &str = "gild-branch";
/// The branch for a repository that has none yet.
pub const DEFAULT_BRANCH: &str = "refs/heads/main";

/// The channel a repository was made or moved for, if gild marked it.
pub fn mark_of(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join(".git").join(CHANNEL_MARK)).ok().map(|s| s.trim().to_string())
}

/// Mark `dir/.git` as channel `id`'s (making `.git` if it isn't there yet).
pub fn mark(dir: &Path, id: &str) -> Result<(), String> {
    let git = dir.join(".git");
    std::fs::create_dir_all(&git).map_err(|e| err("marking the channel's history", e))?;
    std::fs::write(git.join(CHANNEL_MARK), id).map_err(|e| err("marking the channel's history", e))
}

/// A branch name gild commits to: `refs/heads/<something valid>`.
pub fn branch_ok(name: &str) -> bool {
    name.strip_prefix("refs/heads/").is_some_and(|b| !b.is_empty() && gix::validate::reference::name(name.into()).is_ok() && !b.contains(".."))
}

/// The branch gild commits to in this repository: the one HEAD was on when
/// gild first used it, recorded in `.git/gild-branch`.
fn branch(repo: &gix::Repository) -> String {
    if let Ok(s) = std::fs::read_to_string(repo.git_dir().join(BRANCH_FILE)) {
        if branch_ok(s.trim()) {
            return s.trim().to_string();
        }
    }
    let name = repo.head_name().ok().flatten().map(|n| n.as_bstr().to_string()).filter(|n| branch_ok(n)).unwrap_or_else(|| DEFAULT_BRANCH.into());
    if repo.workdir().is_some() {
        let _ = std::fs::write(repo.git_dir().join(BRANCH_FILE), &name);
    }
    name
}

/// Record the branch gild commits to (from `gild.json`), unless one is.
pub fn set_branch(dir: &Path, name: &str) -> Result<(), String> {
    if !branch_ok(name) {
        return Err(format!("{name} isn't a branch name gild uses."));
    }
    let file = dir.join(".git").join(BRANCH_FILE);
    if !file.exists() {
        std::fs::write(file, name).map_err(|e| err("recording the channel's branch", e))?;
    }
    Ok(())
}

/// The branch gild commits to in the repository in `dir`.
pub fn branch_of(dir: &Path) -> Option<String> {
    open(dir).ok().flatten().map(|r| branch(&r))
}

/// The folder must be on gild's branch: if someone checked out another one,
/// gild neither commits there nor takes that branch's files in.
fn on_branch(repo: &gix::Repository) -> Result<(), String> {
    if repo.workdir().is_none() {
        return Ok(());
    }
    let want = branch(repo);
    let now = repo.head_name().ok().flatten().map(|n| n.as_bstr().to_string());
    if now.as_deref() == Some(want.as_str()) {
        return Ok(());
    }
    let short = want.trim_start_matches("refs/heads/");
    let at = now.map(|n| n.trim_start_matches("refs/heads/").to_string()).unwrap_or_else(|| "a commit, not a branch".into());
    Err(format!("The folder is on {at}, but gild keeps this channel on {short}. Switch back (git checkout {short}) and gild carries on."))
}

/// `[core]` for a repository gild made or moved: it has files (not bare),
/// files are written byte for byte (no line-ending changes, whatever the
/// computer's git is set to), and a link in the history is checked out as
/// a plain file holding its target, never as a link that writes could
/// follow out of the folder.
const OWN_CORE: &str = "\tbare = false\n\tautocrlf = false\n\tsymlinks = false\n";

/// Settings for a repository gild made or moved (`OWN_CORE`), and file
/// managers' junk ignored.
fn own_config(git_dir: &Path) -> Result<(), String> {
    let path = git_dir.join("config");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| "[core]\n".into());
    let mut out = String::new();
    let mut core = false;
    for line in text.lines() {
        let key = line.trim().split('=').next().unwrap_or("").trim().to_ascii_lowercase();
        if line.trim().starts_with('[') {
            core = line.trim().eq_ignore_ascii_case("[core]");
        } else if core && (key == "bare" || key == "autocrlf" || key == "symlinks") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
        if core && line.trim().eq_ignore_ascii_case("[core]") {
            out.push_str(OWN_CORE);
        }
    }
    if !text.lines().any(|l| l.trim().eq_ignore_ascii_case("[core]")) {
        out.push_str("[core]\n");
        out.push_str(OWN_CORE);
    }
    std::fs::write(&path, out).map_err(|e| err("setting up the channel's history", e))?;
    let info = git_dir.join("info");
    std::fs::create_dir_all(&info).map_err(|e| err("setting up the channel's history", e))?;
    let exclude = info.join("exclude");
    let had = std::fs::read_to_string(&exclude).unwrap_or_default();
    if !had.contains(".DS_Store") {
        std::fs::write(&exclude, format!("{had}{}{JUNK}", if had.is_empty() || had.ends_with('\n') { "" } else { "\n" }))
            .map_err(|e| err("setting up the channel's history", e))?;
    }
    Ok(())
}

/// The tip of gild's branch (not whatever HEAD points at).
fn head(repo: &gix::Repository) -> Option<ObjectId> {
    let mut r = repo.find_reference(branch(repo).as_str()).ok()?;
    r.peel_to_id().ok().map(|id| id.detach())
}

/// The commit a channel's repository is at, if it has one.
pub fn head_commit(dir: &Path) -> Result<Option<String>, String> {
    let Some(repo) = open(dir)? else { return Ok(None) };
    Ok(head(&repo).map(|id| id.to_string()))
}

/// Write `files` (path → bytes) as one commit by `author`, and return its id.
pub fn commit(dir: &Path, author: &str, message: &str, files: &[(String, Vec<u8>)]) -> Result<String, String> {
    let repo = open_existing(dir)?;
    on_branch(&repo)?;
    let changes: Vec<(String, Option<Vec<u8>>)> = files.iter().map(|(p, b)| (p.clone(), Some(b.clone()))).collect();
    unchanged_on_disk(&repo, &changes)?;
    let fresh = put_files(&repo, &changes)?;
    let parent = head(&repo);
    let id = commit_on(&repo, &branch(&repo), parent, parent.into_iter().collect(), &changes, author, message)?;
    sync_index(&repo, id, &fresh)?;
    Ok(id.to_string())
}

/// A path in a repository's folder: relative, `/`-separated, never into
/// `.git`, and through no link (a linked `docs` or `.gild`, or a file that
/// is a link, would take gild's reads and writes out of the folder).
fn disk_path(root: &Path, path: &str) -> Result<PathBuf, String> {
    let mut out = root.to_path_buf();
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.eq_ignore_ascii_case(".git") || part.contains('\\') {
            return Err(format!("{path} isn't a path gild writes."));
        }
        out.push(part);
        if std::fs::symlink_metadata(&out).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!("{path} goes through a link, so gild doesn't read or write it."));
        }
    }
    Ok(out)
}

/// The id git gives these bytes as a file.
fn blob_id(repo: &gix::Repository, bytes: &[u8]) -> Result<ObjectId, String> {
    gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, bytes).map_err(|e| err("reading a file", e))
}

/// Before gild writes a file, it must still be what git's index says: a
/// save from another app since gild last looked is never written over.
fn unchanged_on_disk(repo: &gix::Repository, changes: &[(String, Option<Vec<u8>>)]) -> Result<(), String> {
    let Some(root) = repo.workdir() else { return Ok(()) };
    let index = repo.index_or_empty().map_err(|e| err("reading git's index", e))?;
    for (path, _) in changes {
        let file = disk_path(root, path)?;
        let entry = index.entry_by_path(path.as_bytes().into()).map(|e| e.id);
        let disk = match std::fs::symlink_metadata(&file) {
            Ok(m) if m.is_file() => Some(blob_id(repo, &std::fs::read(&file).map_err(|e| err(&format!("reading {path}"), e))?)?),
            Ok(_) => return Err(format!("{path} isn't a file on disk, so gild didn't write it.")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(err(&format!("reading {path}"), e)),
        };
        if entry != disk {
            return Err(format!("{path} changed outside gild just now, so gild didn't write over it. Try again."));
        }
    }
    Ok(())
}

fn stat_of(path: &Path) -> Result<gix::index::entry::Stat, String> {
    let meta = gix::index::fs::Metadata::from_path_no_follow(path).map_err(|e| err("reading a file's details", e))?;
    gix::index::entry::Stat::from_fs(&meta).map_err(|e| err("reading a file's details", e))
}

/// Write the changes into the repository's folder (none for a bare one),
/// and return each written file's details as they are now.
fn put_files(repo: &gix::Repository, changes: &[(String, Option<Vec<u8>>)]) -> Result<HashMap<String, gix::index::entry::Stat>, String> {
    let mut fresh = HashMap::new();
    let Some(root) = repo.workdir() else { return Ok(fresh) };
    for (path, bytes) in changes {
        let file = disk_path(root, path)?;
        match bytes {
            Some(bytes) => {
                if let Some(dir) = file.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| err(&format!("writing {path}"), e))?;
                }
                std::fs::write(&file, bytes).map_err(|e| err(&format!("writing {path}"), e))?;
                // Its details count only if the file is still what gild
                // wrote: another app saving now must still show as a change.
                let stat = stat_of(&file)?;
                if std::fs::read(&file).ok().as_deref() == Some(bytes.as_slice()) {
                    fresh.insert(path.clone(), stat);
                }
            }
            None => {
                if file.exists() {
                    std::fs::remove_file(&file).map_err(|e| err(&format!("removing {path}"), e))?;
                }
            }
        }
    }
    Ok(fresh)
}

/// Make git's index the tree of `commit`. A file's details (size, times)
/// come from `fresh` when gild just wrote or read it, else from the old
/// index if the file didn't change; otherwise they're left empty, so git
/// compares that file's bytes the next time it looks.
fn sync_index(repo: &gix::Repository, commit: ObjectId, fresh: &HashMap<String, gix::index::entry::Stat>) -> Result<(), String> {
    if repo.workdir().is_none() {
        return Ok(());
    }
    let tree = tree_of(repo, commit)?.id;
    let mut index = repo.index_from_tree(&tree).map_err(|e| err("updating git's index", e))?;
    let old = repo.open_index().ok();
    for (entry, path) in index.entries_mut_with_paths() {
        let key = path.to_string();
        if let Some(stat) = fresh.get(&key) {
            entry.stat = *stat;
        } else if let Some(o) = old.as_ref().and_then(|o| o.entry_by_path(path)) {
            if o.id == entry.id && o.mode == entry.mode {
                entry.stat = o.stat;
            }
        }
    }
    index.write(Default::default()).map_err(|e| err("updating git's index", e))?;
    Ok(())
}

/// The tree of `base` with `changes` (path → new bytes, or None: removed).
fn tree_with(repo: &gix::Repository, base: Option<ObjectId>, changes: &[(String, Option<Vec<u8>>)]) -> Result<ObjectId, String> {
    let base = match base {
        Some(id) => repo
            .find_commit(id)
            .and_then(|c| c.tree_id())
            .map_err(|e| err("reading the channel's last change", e))?
            .detach(),
        None => repo.empty_tree().id,
    };
    let mut editor = repo.edit_tree(base).map_err(|e| err("preparing the change", e))?;
    for (path, bytes) in changes {
        match bytes {
            Some(bytes) => {
                let blob = repo.write_blob(bytes).map_err(|e| err("writing a file", e))?;
                editor.upsert(path.as_str(), EntryKind::Blob, blob).map_err(|e| err("adding a file", e))?;
            }
            None => {
                editor.remove(path.as_str()).map_err(|e| err("removing a file", e))?;
            }
        }
    }
    Ok(editor.write().map_err(|e| err("writing the change", e))?.detach())
}

/// One commit: `base`'s tree with `changes` written over it, with
/// `parents`, moving `reference` to it.
fn commit_on(
    repo: &gix::Repository,
    reference: &str,
    base: Option<ObjectId>,
    parents: Vec<ObjectId>,
    changes: &[(String, Option<Vec<u8>>)],
    author: &str,
    message: &str,
) -> Result<ObjectId, String> {
    let tree = tree_with(repo, base, changes)?;
    commit_tree(repo, reference, tree, parents, author, message)
}

fn commit_tree(repo: &gix::Repository, reference: &str, tree: ObjectId, parents: Vec<ObjectId>, author: &str, message: &str) -> Result<ObjectId, String> {
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
pub fn snapshot(dir: &Path) -> Result<Snapshot, String> {
    let empty = || Snapshot { commit: None, acl: Acl::default(), view: None, records: vec![], docs: vec![] };
    let Some(repo) = open(dir)? else { return Ok(empty()) };
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
/// `.gild/` folder (the table's records, access, the view's settings) and,
/// in a workspace's main repository, `gild.json` and `.gitignore`.
pub fn files(dir: &Path) -> Result<Vec<FileEntry>, String> {
    let Some(repo) = open(dir)? else { return Ok(Vec::new()) };
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
        if prefix.is_empty() && [".gild", HIDDEN_MANIFEST, ".gitignore"].contains(&name.as_str()) {
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
pub fn read_file(dir: &Path, path: &str) -> Result<Option<Vec<u8>>, String> {
    let Some(repo) = open(dir)? else { return Ok(None) };
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
pub fn propose(dir: &Path, id: &str, author: &str, message: &str, path: &str, body: &[u8]) -> Result<(), String> {
    let repo = open_existing(dir)?;
    let base = head(&repo).ok_or("This channel has no history to propose a change to.")?;
    let files = [(path.to_string(), Some(body.to_vec()))];
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
pub fn proposals(dir: &Path) -> Result<Vec<RawProposal>, String> {
    let Some(repo) = open(dir)? else { return Ok(vec![]) };
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
pub fn approve(dir: &Path, id: &str, author: &str, message: &str) -> Result<(), String> {
    let repo = open(dir)?.ok_or("This channel has no history.")?;
    on_branch(&repo)?;
    let (commit, parent, path) = proposal_parts(&repo, id)?;
    let current = head(&repo).ok_or("This channel has no history.")?;
    if blob_at(&repo, Some(parent), &path)? != blob_at(&repo, Some(current), &path)? {
        return Err("The doc changed after this was proposed. Reject it and ask for a fresh proposal.".into());
    }
    let body = text_of(&repo, blob_at(&repo, Some(commit), &path)?)?.unwrap_or_default();
    let changes = [(path, Some(body.into_bytes()))];
    unchanged_on_disk(&repo, &changes)?;
    let fresh = put_files(&repo, &changes)?;
    let merged = commit_on(&repo, &branch(&repo), Some(current), vec![current, commit], &changes, author, message)?;
    sync_index(&repo, merged, &fresh)?;
    delete_proposal(&repo, id)
}

/// Close a proposal without taking it. Its commit stays reachable from
/// `refs/gild/rejected/<id>`, so the history keeps what was proposed.
pub fn reject(dir: &Path, id: &str) -> Result<(), String> {
    let repo = open(dir)?.ok_or("This channel has no history.")?;
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
pub fn history(dir: &Path, path: &str) -> Result<Vec<Change>, String> {
    let Some(repo) = open(dir)? else { return Ok(vec![]) };
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

/// How a file in a channel's folder differs from its last commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Edit {
    Changed,
    Added,
    Removed,
}

/// What differs between a repository's folder and git's index, as git sees
/// it (`.gitignore` and `.git/info/exclude` apply). In a workspace's main
/// repository the other channels' folders aren't its files.
fn folder_edits(repo: &gix::Repository, main: bool) -> Result<Vec<(String, Edit)>, String> {
    use gix::status::index_worktree::Item;
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};
    let mut out = Vec::new();
    if repo.workdir().is_none() {
        return Ok(out);
    }
    let platform = repo
        .status(gix::progress::Discard)
        .map_err(|e| err("looking at the channel's folder", e))?
        .untracked_files(gix::status::UntrackedFiles::Files)
        .index_worktree_rewrites(None);
    for item in platform.into_index_worktree_iter(Vec::new()).map_err(|e| err("looking at the channel's folder", e))? {
        let item = item.map_err(|e| err("looking at the channel's folder", e))?;
        let (path, edit) = match &item {
            Item::Modification { rela_path, status, .. } => match status {
                EntryStatus::Change(Change::Removed) => (rela_path.to_string(), Edit::Removed),
                EntryStatus::Change(_) => (rela_path.to_string(), Edit::Changed),
                _ => continue,
            },
            Item::DirectoryContents { entry, .. } => {
                if entry.status != gix::dir::entry::Status::Untracked || entry.disk_kind == Some(gix::dir::entry::Kind::Repository) {
                    continue;
                }
                (entry.rela_path.to_string(), Edit::Added)
            }
            Item::Rewrite { .. } => continue,
        };
        if main && (path == CHANNELS_DIR || path.starts_with(&format!("{CHANNELS_DIR}/"))) {
            continue;
        }
        out.push((path, edit));
    }
    out.sort();
    Ok(out)
}

/// Everything in a repository that doesn't match its last commit: files on
/// disk that differ from git's index, and index entries that differ from
/// HEAD. Empty means `git status` is clean.
pub fn status(dir: &Path, main: bool) -> Result<Vec<String>, String> {
    let Some(repo) = open(dir)? else { return Ok(vec![]) };
    let mut out: Vec<String> = folder_edits(&repo, main)?.into_iter().map(|(p, e)| format!("{e:?}: {p}")).collect();
    if let Err(e) = on_branch(&repo) {
        out.push(e);
    }
    if repo.workdir().is_some() {
        let index = repo.index_or_empty().map_err(|e| err("reading git's index", e))?;
        let want = match head(&repo) {
            Some(id) => repo.index_from_tree(&tree_of(&repo, id)?.id).map_err(|e| err("reading history", e))?,
            None => gix::index::File::from_state(gix::index::State::new(repo.object_hash()), repo.index_path()),
        };
        let entries = |f: &gix::index::State| -> BTreeMap<String, (ObjectId, u32)> {
            f.entries_with_paths_by_filter_map(|_, e| Some((e.id, e.mode.bits()))).map(|(p, v)| (p.to_string(), v)).collect()
        };
        let (have, want) = (entries(&index), entries(&want));
        for (path, v) in &want {
            if have.get(path) != Some(v) {
                out.push(format!("Not in the index: {path}"));
            }
        }
        out.extend(have.keys().filter(|p| !want.contains_key(*p)).map(|p| format!("Only in the index: {p}")));
    }
    Ok(out)
}

/// gild's own files must parse: a broken one is never committed.
fn check_gild_file(path: &str, bytes: &[u8]) -> Result<(), String> {
    let parsed = if path == ACL_PATH {
        serde_json::from_slice::<Acl>(bytes).map(|_| ())
    } else if path == VIEW_PATH {
        serde_json::from_slice::<View>(bytes).map(|_| ())
    } else if path.starts_with(&format!("{RECORDS_DIR}/")) && path.ends_with(".json") {
        serde_json::from_slice::<Record>(bytes).map(|_| ())
    } else if path == MANIFEST {
        serde_json::from_slice::<serde_json::Value>(bytes).map(|_| ())
    } else {
        return Ok(());
    };
    parsed.map_err(|e| e.to_string())
}

/// The biggest file gild takes into a history from outside (it reads the
/// whole file while the workspace waits).
pub const MAX_OUTSIDE_FILE: u64 = 50_000_000;

/// What `check_outside` did.
#[derive(Debug, Default, PartialEq)]
pub struct Outside {
    /// The commit it made, if any.
    pub commit: Option<String>,
    pub paths: Vec<String>,
    /// Files it left as they are, and why, in plain words.
    pub problems: Vec<String>,
}

/// Commit what changed in the channel's folder outside gild (another
/// editor, a file copied in) as one commit by `author`. A broken gild file
/// (`.gild/*.json`) and links are left out and named in `problems`.
pub fn check_outside(dir: &Path, author: &str, main: bool) -> Result<Outside, String> {
    let mut out = Outside::default();
    let Some(repo) = open(dir)? else { return Ok(out) };
    let Some(root) = repo.workdir().map(Path::to_path_buf) else { return Ok(out) };
    // Another branch checked out: its files aren't this channel's to take in.
    if let Err(e) = on_branch(&repo) {
        out.problems.push(e);
        return Ok(out);
    }
    let edits = folder_edits(&repo, main)?;
    if edits.is_empty() {
        return Ok(out);
    }
    let mut changes = Vec::new();
    let mut fresh = HashMap::new();
    for (path, edit) in edits {
        let file = match disk_path(&root, &path) {
            Ok(f) => f,
            Err(e) => {
                out.problems.push(format!("{e} It's left out."));
                continue;
            }
        };
        if edit == Edit::Removed {
            changes.push((path, None));
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&file) else { continue };
        if meta.file_type().is_symlink() {
            out.problems.push(format!("{path} is a link: gild keeps files, not links, so it left it out."));
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        if meta.len() > MAX_OUTSIDE_FILE {
            out.problems.push(format!("{path} is bigger than {} MB, so gild left it out of the channel's history.", MAX_OUTSIDE_FILE / 1_000_000));
            continue;
        }
        // The details before the bytes: a change while reading shows next time.
        let stat = stat_of(&file)?;
        let bytes = std::fs::read(&file).map_err(|e| err(&format!("reading {path}"), e))?;
        if let Err(e) = check_gild_file(&path, &bytes) {
            out.problems.push(format!("{path} isn't valid ({e}), so gild left it as it is. Fix it or undo the change."));
            continue;
        }
        fresh.insert(path.clone(), stat);
        changes.push((path, Some(bytes)));
    }
    if changes.is_empty() {
        return Ok(out);
    }
    let parent = head(&repo);
    let tree = tree_with(&repo, parent, &changes)?;
    let base = match parent {
        Some(p) => Some(tree_of(&repo, p)?.id),
        None => None,
    };
    if base == Some(tree) {
        // The files are what the last commit says (an index that fell
        // behind): nothing to commit, only the index to bring up to date.
        if let Some(p) = parent {
            sync_index(&repo, p, &fresh)?;
        }
        return Ok(out);
    }
    out.paths = changes.iter().map(|(p, _)| p.clone()).collect();
    let named: Vec<&str> = out.paths.iter().take(5).map(String::as_str).collect();
    let more = out.paths.len().saturating_sub(named.len());
    let message = format!("Edit outside gild: {}{}", named.join(", "), if more > 0 { format!(" and {more} more") } else { String::new() });
    let id = commit_tree(&repo, &branch(&repo), tree, parent.into_iter().collect(), author, &message)?;
    sync_index(&repo, id, &fresh)?;
    out.commit = Some(id.to_string());
    Ok(out)
}

/// Copy a folder. Symbolic links are skipped, never followed: a folder
/// someone hands you can't pull other files from your computer in.
pub fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| err("copying history", e))?;
    for entry in std::fs::read_dir(from).map_err(|e| err("copying history", e))? {
        let entry = entry.map_err(|e| err("copying history", e))?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type().map_err(|e| err("copying history", e))?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            // Copying again over an earlier copy (a move that stopped
            // halfway): git's objects are read-only and never change, so
            // what's already there and the same stays; anything else is
            // replaced. A file that can't be read is a failure, never "the same".
            if target.exists() {
                let source = std::fs::read(entry.path()).map_err(|e| err("copying history", e))?;
                if std::fs::read(&target).map_err(|e| err("copying history", e))? == source {
                    continue;
                }
                writable(&target)?;
                std::fs::remove_file(&target).map_err(|e| err("copying history", e))?;
            }
            std::fs::copy(entry.path(), &target).map_err(|e| err("copying history", e))?;
        }
    }
    Ok(())
}

/// Windows refuses to delete a read-only file (git's objects are), so the
/// flag goes first. Elsewhere deleting depends on the folder, not the file.
#[cfg(windows)]
fn writable(file: &Path) -> Result<(), String> {
    let mut p = std::fs::metadata(file).map_err(|e| err("copying history", e))?.permissions();
    p.set_readonly(false);
    std::fs::set_permissions(file, p).map_err(|e| err("copying history", e))
}

#[cfg(not(windows))]
fn writable(_: &Path) -> Result<(), String> {
    Ok(())
}

/// Every ref and where it points (HEAD as the branch it names), sorted.
pub fn refs(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let Some(repo) = open(dir)? else { return Ok(vec![]) };
    let mut out = Vec::new();
    let platform = repo.references().map_err(|e| err("reading refs", e))?;
    for r in platform.all().map_err(|e| err("reading refs", e))? {
        let r = r.map_err(|e| err("reading refs", e))?;
        let to = match r.target() {
            gix::refs::TargetRef::Object(id) => id.to_string(),
            gix::refs::TargetRef::Symbolic(name) => format!("ref: {}", name.as_bstr()),
        };
        out.push((r.name().as_bstr().to_string(), to));
    }
    let head = repo.head_name().map_err(|e| err("reading HEAD", e))?.map(|n| n.as_bstr().to_string());
    out.push(("HEAD".into(), head.map(|n| format!("ref: {n}")).unwrap_or_else(|| self::head(&repo).map(|i| i.to_string()).unwrap_or_default())));
    out.sort();
    Ok(out)
}

/// The tree HEAD's commit has, if there is one.
pub fn head_tree(dir: &Path) -> Result<Option<String>, String> {
    let Some(repo) = open(dir)? else { return Ok(None) };
    match head(&repo) {
        Some(id) => Ok(Some(tree_of(&repo, id)?.id.to_string())),
        None => Ok(None),
    }
}

/// A repository in `dir` with the history in `git_dir` (a bare repository,
/// or another one's `.git`), and HEAD's files checked out over whatever is
/// there. Only the history is copied (objects, refs, packed-refs, HEAD):
/// never another repository's config, hooks or `objects/info/alternates`
/// (which would read objects from elsewhere on this computer). The copy
/// gets a config of gild's own (`fresh_config`).
pub fn copy_repo(git_dir: &Path, dir: &Path) -> Result<(), String> {
    if !is_git_dir(git_dir) {
        return Err(format!("{} isn't a git repository's history.", git_dir.display()));
    }
    let head = std::fs::read_to_string(git_dir.join("HEAD")).map_err(|e| err("reading the history's HEAD", e))?;
    let head = head.trim();
    let named = head.strip_prefix("ref: ").is_some_and(branch_ok);
    if !named && !(head.len() == 40 && head.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err(format!("{}'s HEAD isn't a branch or a commit.", git_dir.display()));
    }
    let target = dir.join(".git");
    for part in ["objects", "refs"] {
        let from = git_dir.join(part);
        if std::fs::symlink_metadata(&from).is_ok_and(|m| m.is_dir()) {
            copy_dir(&from, &target.join(part))?;
        }
    }
    for f in ["objects/info/alternates", "objects/info/http-alternates"] {
        let _ = std::fs::remove_file(target.join(f));
    }
    if std::fs::symlink_metadata(git_dir.join("packed-refs")).is_ok_and(|m| m.is_file()) {
        std::fs::copy(git_dir.join("packed-refs"), target.join("packed-refs")).map_err(|e| err("copying history", e))?;
    }
    std::fs::write(target.join("HEAD"), format!("{head}\n")).map_err(|e| err("copying history", e))?;
    fresh_config(&target)?;
    checkout_head(dir)
}

/// Write every file of HEAD's tree into the folder and make git's index
/// match: what `git clone` does after it has the history.
pub fn checkout_head(dir: &Path) -> Result<(), String> {
    let repo = open_existing(dir)?;
    let Some(root) = repo.workdir().map(Path::to_path_buf) else { return Err("The channel's history has no folder to check out into.".into()) };
    let Some(id) = head(&repo) else { return Ok(()) };
    let mut index = repo.index_from_tree(&tree_of(&repo, id)?.id).map_err(|e| err("reading history", e))?;
    let mut options = repo
        .checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)
        .map_err(|e| err("checking out the channel's files", e))?;
    options.destination_is_initially_empty = false;
    options.overwrite_existing = true;
    // A link in the history becomes a file holding its target (as
    // `core.symlinks = false` says), never a link out of the folder.
    options.fs.symlink = false;
    let objects = repo.objects.clone().into_arc().map_err(|e| err("checking out the channel's files", e))?;
    let outcome = gix::worktree::state::checkout(
        &mut index,
        root,
        objects,
        &gix::progress::Discard,
        &gix::progress::Discard,
        &std::sync::atomic::AtomicBool::new(false),
        options,
    )
    .map_err(|e| err("checking out the channel's files", e))?;
    if let Some(e) = outcome.errors.first() {
        return Err(format!("checking out {}: {}", e.path, e.error));
    }
    if let Some(c) = outcome.collisions.first() {
        return Err(format!("checking out {}: something else is in the way", c.path));
    }
    index.write(Default::default()).map_err(|e| err("updating git's index", e))?;
    Ok(())
}

/// Read every object reachable from every ref: commits, their trees and
/// parents, and every file. Any that is missing or unreadable is an error.
pub fn every_object_readable(dir: &Path) -> Result<usize, String> {
    let repo = open_existing(dir)?;
    let mut todo: Vec<ObjectId> = Vec::new();
    let platform = repo.references().map_err(|e| err("reading refs", e))?;
    for r in platform.all().map_err(|e| err("reading refs", e))? {
        let mut r = r.map_err(|e| err("reading refs", e))?;
        todo.push(r.peel_to_id().map_err(|e| err(&format!("reading {}", r.name().as_bstr()), e))?.detach());
    }
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = todo.pop() {
        if !seen.insert(id) {
            continue;
        }
        let object = repo.find_object(id).map_err(|e| err(&format!("reading object {id}"), e))?;
        match object.kind {
            gix::objs::Kind::Commit => {
                let c = object.into_commit();
                todo.push(c.tree_id().map_err(|e| err(&format!("reading commit {id}"), e))?.detach());
                todo.extend(c.parent_ids().map(|p| p.detach()));
            }
            gix::objs::Kind::Tree => {
                for e in object.into_tree().iter() {
                    let e = e.map_err(|e| err(&format!("reading tree {id}"), e))?;
                    // A submodule's commit lives in another repository.
                    if !e.mode().is_commit() {
                        todo.push(e.object_id());
                    }
                }
            }
            gix::objs::Kind::Tag => {
                let t = object.into_tag();
                todo.push(t.target_id().map_err(|e| err(&format!("reading tag {id}"), e))?.detach());
            }
            gix::objs::Kind::Blob => {}
        }
    }
    Ok(seen.len())
}

/// Whether the repository in `dir` has exactly `old`'s history (every ref,
/// HEAD's tree, every object those reach, readable) and a clean folder.
pub fn same_history(old: &Path, dir: &Path, main: bool) -> Result<(), String> {
    if refs(old)? != refs(dir)? {
        return Err("its branches don't match the old history".into());
    }
    if head_tree(old)? != head_tree(dir)? {
        return Err("its files don't match the old history".into());
    }
    let (had, has) = (every_object_readable(old)?, every_object_readable(dir).map_err(|e| format!("its history is incomplete ({e})"))?);
    if had != has {
        return Err(format!("its history has {has} objects, the old one {had}"));
    }
    let left = status(dir, main)?;
    if !left.is_empty() {
        return Err(format!("its folder doesn't match its history ({})", left.join(", ")));
    }
    Ok(())
}
