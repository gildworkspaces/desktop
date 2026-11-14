//! Where each workspace lives: a folder of ordinary git repositories that a
//! person can open, `~/Gild/<name>/` unless they pick another place.
//!
//!   <workspace>/               the main repository: the workspace and #general
//!   ├─ gild.json               the workspace: id, name, org, its channels
//!   ├─ .gitignore              lists channels/
//!   ├─ .gild/, docs/, files    #general's (see records.rs)
//!   └─ channels/<name>/        every other channel, a repository of its own
//!
//! SQLite (in the app's data folder) records where each workspace's folder
//! is and which folder each channel has. Live chat stays in SQLite: it never
//! goes in git.
//!
//! Before 0.24 each channel's history was a bare repository in the data
//! folder (`channels/<id>.git`). `ensure_folders` moves them on first launch:
//! copy, check out, check that every ref and HEAD's tree match, and only
//! then delete the old one. Each step can run again, so a move that was
//! interrupted is finished on the next launch.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use turso_core::Value;

use super::{err, text, Channel, Store};
use crate::records;

/// The main channel's folder: the workspace's folder itself.
pub const MAIN: &str = ".";

/// Where the old histories go once moved (kept, not deleted, in 0.24).
pub const MIGRATED: &str = "migrated-0.24";

/// `gild.json`, at the root of a workspace's main repository.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WorkspaceFile {
    /// The format of this file.
    pub gild: u32,
    pub id: String,
    pub name: String,
    pub org: Option<String>,
    pub channels: Vec<ChannelEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ChannelEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub topic: String,
    /// Relative to the workspace's folder: `.` for the main channel,
    /// `channels/<name>` for every other.
    pub folder: String,
    /// Where the channel's repository is shared from; none yet.
    #[serde(default)]
    pub remote: Option<String>,
    /// The branch gild commits to (`refs/heads/main` unless the repository
    /// was on another when gild first used it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// Whether `id` has the shape `new_id` gives (22 lower-case hex digits).
/// Ids name files (`<id>.git`), so anything else from a file someone hands
/// gild is refused.
pub fn valid_id(id: &str) -> bool {
    id.len() == 22 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// What a folder name is to a file system that ignores case and Unicode
/// form (APFS, NTFS): two names with the same identity are one folder.
pub fn identity(folder: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    folder
        .nfc()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'ς' => 'σ',
            'ſ' => 's',
            'ı' => 'i',
            'µ' => 'μ',
            c => c,
        })
        .nfc()
        .collect()
}

/// Names Windows keeps for devices: never a file or folder name.
fn reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim_end().to_uppercase();
    let numbered = |p: &str| stem.strip_prefix(p).is_some_and(|n| n.chars().count() == 1 && n.chars().all(|c| c.is_ascii_digit() || "¹²³".contains(c)));
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered("COM") || numbered("LPT")
}

/// A folder name from anything a person typed: letters, digits, spaces, `-`
/// and `_`, nothing that leaves the folder it's in, and nothing Windows
/// refuses.
pub fn folder_name(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '-' }).collect();
    let s = s.trim().trim_start_matches(['.', '-']).trim().to_string();
    if s.is_empty() {
        "untitled".into()
    } else if reserved(&s) {
        format!("{s} workspace")
    } else {
        s
    }
}

/// A channel's folder in its workspace's (not the main channel's).
pub fn channel_folder(name: &str) -> String {
    let name = if reserved(name) { format!("_{name}") } else { name.to_string() };
    format!("{}/{name}", records::CHANNELS_DIR)
}

/// A channel's folder on disk.
pub fn join(root: &Path, folder: &str) -> PathBuf {
    folder.split('/').filter(|p| *p != MAIN).fold(root.to_path_buf(), |p, part| p.join(part))
}

/// Which channel is the main one: #general, or the first.
pub fn main_index<'a>(names: impl Iterator<Item = &'a str>) -> usize {
    names.enumerate().find(|(_, n)| *n == "general").map(|(i, _)| i).unwrap_or(0)
}

/// `folder` in the workspace's folder `root`, checked: no link on the way
/// (`channels`, `channels/<name>`, its `.git`), really inside `root` once
/// links are resolved, and not marked as another channel's (`id`: the one
/// expected; "" for a folder that mustn't be anyone's yet).
pub fn safe_dir(root: &Path, folder: &str, id: &str) -> Result<PathBuf, String> {
    let dir = join(root, folder);
    let mut at = root.to_path_buf();
    for part in folder.split('/').filter(|p| *p != MAIN) {
        at.push(part);
        if let Ok(m) = std::fs::symlink_metadata(&at) {
            if m.file_type().is_symlink() || !m.is_dir() {
                return Err(format!("{} in the workspace's folder is a link or a file, not a folder, so gild won't use it.", at.display()));
            }
        }
    }
    if std::fs::symlink_metadata(dir.join(".git")).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(format!("{}'s .git is a link, so gild won't use it.", dir.display()));
    }
    if dir.exists() {
        let (inside, top) = (dir.canonicalize().map_err(|e| err("finding the channel's folder", e))?, root.canonicalize().map_err(|e| err("finding the workspace's folder", e))?);
        if !inside.starts_with(&top) {
            return Err(format!("{} isn't inside the workspace's folder, so gild won't use it.", dir.display()));
        }
    }
    if let Some(mark) = records::mark_of(&dir) {
        if mark != id {
            return Err(format!("{} holds another channel's history, so gild won't use it for this one.", dir.display()));
        }
    }
    Ok(dir)
}

fn path_text(path: &Path) -> Result<String, String> {
    path.to_str().map(str::to_string).ok_or_else(|| format!("gild can't keep a workspace at {}: its path isn't valid text.", path.display()))
}

// Pause points for tests: the move stops there as if gild were quit.
#[cfg(test)]
thread_local! {
    pub(crate) static STOP_AT: std::cell::RefCell<Vec<&'static str>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn stop_here(point: &'static str) -> Result<(), String> {
    #[cfg(test)]
    {
        let stop = STOP_AT.with(|s| {
            let mut s = s.borrow_mut();
            let at = s.iter().position(|p| *p == point);
            at.map(|i| s.remove(i)).is_some()
        });
        if stop {
            return Err(format!("stopped at {point}"));
        }
    }
    let _ = point;
    Ok(())
}

impl Store {
    /// Where a workspace's folder is.
    pub fn workspace_folder(&self, workspace_id: &str) -> Result<PathBuf, String> {
        self.workspace_root(workspace_id)?.ok_or_else(|| "That workspace no longer exists.".into())
    }

    fn workspace_root(&self, workspace_id: &str) -> Result<Option<PathBuf>, String> {
        Ok(self
            .query("SELECT path FROM workspace_folders WHERE workspace_id = ?1", &[Value::from_text(workspace_id.to_string())])?
            .first()
            .map(|r| PathBuf::from(text(&r[0]))))
    }

    pub(super) fn set_workspace_folder(&self, workspace_id: &str, root: &Path) -> Result<(), String> {
        self.run(
            "INSERT INTO workspace_folders (workspace_id, path) VALUES (?1, ?2)",
            &[Value::from_text(workspace_id.to_string()), Value::from_text(path_text(root)?)],
        )
    }

    fn folder_of(&self, channel_id: &str) -> Result<Option<String>, String> {
        Ok(self
            .query("SELECT folder FROM channel_folders WHERE channel_id = ?1", &[Value::from_text(channel_id.to_string())])?
            .first()
            .map(|r| text(&r[0])))
    }

    pub(super) fn set_channel_folder(&self, channel_id: &str, folder: &str) -> Result<(), String> {
        self.run(
            "INSERT INTO channel_folders (channel_id, folder) VALUES (?1, ?2)",
            &[Value::from_text(channel_id.to_string()), Value::from_text(folder.to_string())],
        )
    }

    /// Every workspace's folder.
    pub fn workspace_folders(&self) -> Result<Vec<(String, PathBuf)>, String> {
        Ok(self
            .query("SELECT workspace_id, path FROM workspace_folders", &[])?
            .iter()
            .map(|r| (text(&r[0]), PathBuf::from(text(&r[1]))))
            .collect())
    }

    /// The name of the workspace kept in `root`, if one is.
    pub(super) fn workspace_at(&self, root: &Path) -> Result<Option<String>, String> {
        let same = |a: &Path, b: &Path| a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y);
        for (id, path) in self.workspace_folders()? {
            if same(&path, root) {
                return Ok(self.workspaces()?.into_iter().find(|w| w.id == id).map(|w| w.name));
            }
        }
        Ok(None)
    }

    /// A folder for a new workspace in `parent`: its name, or "<name> 2"
    /// (and so on) when that's taken.
    pub(super) fn free_folder(&self, parent: &Path, name: &str) -> Result<PathBuf, String> {
        let base = folder_name(name);
        let mut n = 1;
        loop {
            let candidate = if n == 1 { parent.join(&base) } else { parent.join(format!("{base} {n}")) };
            if !candidate.exists() && self.workspace_at(&candidate)?.is_none() {
                return Ok(candidate);
            }
            n += 1;
        }
    }

    /// A channel's old history in the data folder (before 0.24), if it's
    /// still there. Only for ids gild made itself.
    fn legacy_repo(&self, channel_id: &str) -> Option<PathBuf> {
        let path = self.data.join("channels").join(format!("{channel_id}.git"));
        (valid_id(channel_id) && path.exists()).then_some(path)
    }

    /// A channel's repository: its folder in its workspace's, checked (see
    /// `safe_dir`). A missing folder is the channel's problem, never made
    /// again empty.
    pub(crate) fn repo(&self, channel_id: &str) -> Result<PathBuf, String> {
        let row = self.query(
            "SELECT w.path, f.folder, c.name FROM channels c JOIN workspace_folders w ON w.workspace_id = c.workspace_id
             JOIN channel_folders f ON f.channel_id = c.id WHERE c.id = ?1",
            &[Value::from_text(channel_id.to_string())],
        )?;
        let row = row.first().ok_or("That channel no longer exists.")?;
        let (root, folder, name) = (PathBuf::from(text(&row[0])), text(&row[1]), text(&row[2]));
        if !root.is_dir() {
            return Err(format!("This workspace's folder, {}, is gone. Put it back to use the workspace again.", root.display()));
        }
        if let Some(old) = self.legacy_repo(channel_id) {
            return Err(format!("#{name}'s history hasn't moved into the workspace's folder yet (it's still in {}). gild tries again when it starts.", old.display()));
        }
        let dir = safe_dir(&root, &folder, channel_id)?;
        if !records::is_repo(&dir) {
            return Err(format!("#{name}'s folder, {}, is gone. Put it back to use the channel again.", dir.display()));
        }
        Ok(dir)
    }

    fn is_main(&self, channel_id: &str) -> Result<bool, String> {
        Ok(self.folder_of(channel_id)?.as_deref() == Some(MAIN))
    }

    /// Where a new channel called `name` goes in the workspace, checked
    /// before anything is written: the workspace's folder for its first
    /// channel, `channels/<name>` (which mustn't exist yet) for any other.
    pub(super) fn new_channel_folder(&self, workspace_id: &str, name: &str) -> Result<(String, PathBuf), String> {
        let root = self.workspace_folder(workspace_id)?;
        let folder = if self.main_channel(workspace_id)?.is_some() { channel_folder(name) } else { MAIN.to_string() };
        for c in self.channels(workspace_id)? {
            if let Some(f) = self.folder_of(&c.id)? {
                if identity(&f) == identity(&folder) {
                    return Err(format!("#{name} would share a folder with #{} on this computer, which doesn't tell their names apart. Pick another name.", c.name));
                }
            }
        }
        let dir = safe_dir(&root, &folder, "")?;
        if folder == MAIN {
            if dir.join(".git").exists() {
                return Err(format!("{} is already a git repository: pick another place for the workspace.", dir.display()));
            }
        } else if std::fs::symlink_metadata(&dir).is_ok() {
            return Err(format!("There's already a {folder} in the workspace's folder: move it away to make #{name} there."));
        }
        Ok((folder, dir))
    }

    /// Give a new channel the folder `new_channel_folder` checked: its
    /// repository, marked as its.
    pub(super) fn place_channel(&self, channel: &Channel, folder: &str, dir: &Path) -> Result<(), String> {
        self.set_channel_folder(&channel.id, folder)?;
        records::init(dir)?;
        records::mark(dir, &channel.id)
    }

    /// Forget a channel's rows (a create that failed halfway).
    pub(super) fn forget_channel(&self, channel_id: &str) {
        let id = [Value::from_text(channel_id.to_string())];
        for table in ["channel_folders", "views", "records", "indexed", "grants", "docs", "channel_rules"] {
            let _ = self.run(&format!("DELETE FROM {table} WHERE channel_id = ?1"), &id);
        }
        let _ = self.run("DELETE FROM thread_replies WHERE message_id IN (SELECT id FROM messages WHERE channel_id = ?1)", &id);
        let _ = self.run("DELETE FROM messages WHERE channel_id = ?1", &id);
        let _ = self.run("DELETE FROM channels WHERE id = ?1", &id);
    }

    /// Forget a workspace's rows (a create or import that failed halfway).
    /// Its folder and files are left as they are.
    pub(super) fn forget_workspace(&self, workspace_id: &str) {
        for c in self.channels(workspace_id).unwrap_or_default() {
            self.forget_channel(&c.id);
        }
        let id = [Value::from_text(workspace_id.to_string())];
        let _ = self.run("DELETE FROM workspace_folders WHERE workspace_id = ?1", &id);
        let _ = self.run("DELETE FROM org_workspaces WHERE workspace_id = ?1", &id);
        let _ = self.run("DELETE FROM workspaces WHERE id = ?1", &id);
    }

    fn main_channel(&self, workspace_id: &str) -> Result<Option<String>, String> {
        for c in self.channels(workspace_id)? {
            if self.is_main(&c.id)? {
                return Ok(Some(c.id));
            }
        }
        Ok(None)
    }

    /// `gild.json` as the index has the workspace now.
    pub fn workspace_file(&self, workspace_id: &str) -> Result<WorkspaceFile, String> {
        let ws = self.workspaces()?.into_iter().find(|w| w.id == workspace_id).ok_or("That workspace no longer exists.")?;
        let org = self
            .query("SELECT org_id FROM org_workspaces WHERE workspace_id = ?1", &[Value::from_text(workspace_id.to_string())])?
            .first()
            .map(|r| text(&r[0]));
        let mut channels = Vec::new();
        for c in self.channels(workspace_id)? {
            let folder = self.folder_of(&c.id)?.unwrap_or_else(|| channel_folder(&c.name));
            let branch = self.repo(&c.id).ok().and_then(|d| records::branch_of(&d));
            channels.push(ChannelEntry { id: c.id, name: c.name, topic: c.topic, folder, remote: None, branch });
        }
        Ok(WorkspaceFile { gild: 1, id: ws.id, name: ws.name, org, channels })
    }

    /// Commit `gild.json` (and `channels/` in `.gitignore`) into the main
    /// repository when they don't say what the index does.
    pub(super) fn write_manifest(&self, workspace_id: &str) -> Result<(), String> {
        let Some(main) = self.main_channel(workspace_id)? else { return Ok(()) };
        let dir = self.repo(&main)?;
        let mut bytes = serde_json::to_vec_pretty(&self.workspace_file(workspace_id)?).map_err(|e| err("writing gild.json", e))?;
        bytes.push(b'\n');
        let had = records::read_file(&dir, records::MANIFEST)?;
        let mut files = Vec::new();
        if had.as_deref() != Some(bytes.as_slice()) {
            files.push((records::MANIFEST.to_string(), bytes));
        }
        let ignore = String::from_utf8_lossy(&records::read_file(&dir, ".gitignore")?.unwrap_or_default()).into_owned();
        let line = format!("{}/", records::CHANNELS_DIR);
        if !ignore.lines().any(|l| l.trim() == line || l.trim() == format!("/{line}")) {
            let sep = if ignore.is_empty() || ignore.ends_with('\n') { "" } else { "\n" };
            files.push((".gitignore".to_string(), format!("{ignore}{sep}{line}\n").into_bytes()));
        }
        if files.is_empty() {
            return Ok(());
        }
        let message = if had.is_none() { "Describe the workspace in gild.json" } else { "Update gild.json" };
        self.write(&main, &self.me()?, message, &files)
    }

    /// Take in what changed in a channel's folder outside gild: one commit
    /// by the person, then the index. True when something changed (a
    /// commit, or what's wrong with the folder).
    pub fn check_outside(&self, channel_id: &str) -> Result<bool, String> {
        let dir = self.repo(channel_id)?;
        let name = self.channel(channel_id)?.map(|c| c.name).unwrap_or_default();
        let found = records::check_outside(&dir, &self.me()?, self.is_main(channel_id)?)?;
        let problems: Vec<String> = found.problems.iter().map(|p| format!("In #{name}'s folder: {p}")).collect();
        let problems_changed = self.set_problems(channel_id, problems);
        let changed = found.commit.is_some() || problems_changed;
        if found.commit.is_some() {
            self.reindex(channel_id)?;
        }
        Ok(changed)
    }

    /// At start: every channel, as gild wasn't watching while it was closed.
    pub(super) fn check_all_outside(&self) -> Result<(), String> {
        for row in self.query("SELECT id FROM channels", &[])? {
            let id = text(&row[0]);
            if let Err(e) = self.check_outside(&id) {
                // A move that failed already said why: keep that.
                self.add_problem(&id, e);
            }
        }
        Ok(())
    }

    /// Add a problem to a channel's, once.
    pub(crate) fn add_problem(&self, channel_id: &str, problem: String) {
        let mut now = self.folder_problems(channel_id);
        if !now.iter().any(|p| p == &problem) {
            now.push(problem);
            self.set_problems(channel_id, now);
        }
    }

    /// Replace a channel's problems; true when they changed.
    pub(crate) fn set_problems(&self, channel_id: &str, problems: Vec<String>) -> bool {
        let Ok(mut all) = self.problems.lock() else { return false };
        let had = all.get(channel_id).cloned().unwrap_or_default();
        if problems.is_empty() {
            all.remove(channel_id);
        } else {
            all.insert(channel_id.to_string(), problems.clone());
        }
        had != problems
    }

    /// What's wrong with a channel's folder, in plain words (empty: nothing).
    pub fn folder_problems(&self, channel_id: &str) -> Vec<String> {
        self.problems.lock().ok().and_then(|p| p.get(channel_id).cloned()).unwrap_or_default()
    }

    /// The channel a file on disk belongs to, if any: `channels/<name>/…`
    /// is that channel's, anything else in a workspace's folder is the main
    /// channel's. Nothing inside a `.git` counts.
    pub fn channel_at(&self, path: &Path) -> Result<Option<String>, String> {
        for (ws, root) in self.workspace_folders()? {
            let rel = match path.strip_prefix(&root) {
                Ok(r) => r.to_path_buf(),
                Err(_) => match root.canonicalize().ok().and_then(|r| path.strip_prefix(r).ok().map(Path::to_path_buf)) {
                    Some(r) => r,
                    None => continue,
                },
            };
            let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
            if parts.iter().any(|p| p == ".git") {
                return Ok(None);
            }
            if parts.first().map(String::as_str) == Some(records::CHANNELS_DIR) {
                let Some(sub) = parts.get(1) else { return Ok(None) };
                let folder = format!("{}/{sub}", records::CHANNELS_DIR);
                for c in self.channels(&ws)? {
                    if self.folder_of(&c.id)?.as_deref() == Some(folder.as_str()) {
                        return Ok(Some(c.id));
                    }
                }
                return Ok(None);
            }
            return self.main_channel(&ws);
        }
        Ok(None)
    }

    /// Give every workspace its folder, moving histories kept the old way
    /// (`<data>/channels/<id>.git`) into it. Nothing here stops gild from
    /// starting: a channel whose move fails keeps its old history where it
    /// is and shows why. Safe to run again: a move that stopped halfway is
    /// finished, and a finished one is left alone. The old histories are
    /// kept, not deleted, in `<data>/migrated-0.24/`.
    pub(super) fn ensure_folders(&self) -> Result<(), String> {
        let legacy = self.data.join("channels");
        // Still moving: a channel with no history yet may get an empty one.
        let moving = legacy.is_dir();
        for ws in self.workspaces()? {
            if let Err(e) = self.ensure_workspace_folder(&ws.id, &ws.name, moving) {
                for c in self.channels(&ws.id).unwrap_or_default() {
                    self.add_problem(&c.id, e.clone());
                }
            }
        }
        if std::fs::read_dir(&legacy).map(|mut d| d.next().is_none()).unwrap_or(false) {
            let _ = std::fs::remove_dir(&legacy);
        }
        Ok(())
    }

    fn ensure_workspace_folder(&self, workspace_id: &str, name: &str, moving: bool) -> Result<(), String> {
        let channels = self.channels(workspace_id)?;
        let root = match self.workspace_root(workspace_id)? {
            Some(root) => {
                if !root.is_dir() && !channels.iter().any(|c| self.legacy_repo(&c.id).is_some()) {
                    // Moved or deleted by someone: never start it again empty.
                    let gone = format!("This workspace's folder, {}, is gone. Put it back to use the workspace again.", root.display());
                    for c in &channels {
                        self.add_problem(&c.id, gone.clone());
                    }
                    return Ok(());
                }
                root
            }
            None => {
                let root = self.free_folder(&self.home, name)?;
                self.set_workspace_folder(workspace_id, &root)?;
                root
            }
        };
        std::fs::create_dir_all(&root).map_err(|e| err(&format!("creating the folder {}", root.display()), e))?;
        let main = main_index(channels.iter().map(|c| c.name.as_str()));
        // Every channel's folder first, and which one each would land in on
        // a file system that ignores case and Unicode form: two channels that
        // would share one are never moved over each other.
        let mut placed: Vec<(&Channel, String, bool)> = Vec::new();
        let mut seen: std::collections::HashMap<String, String> = Default::default();
        for (i, c) in channels.iter().enumerate() {
            let (folder, new_row) = match self.folder_of(&c.id)? {
                Some(f) => (f, false),
                None => {
                    let f = if i == main { MAIN.to_string() } else { channel_folder(&c.name) };
                    self.set_channel_folder(&c.id, &f)?;
                    (f, true)
                }
            };
            if let Some(first) = seen.get(&identity(&folder)) {
                self.add_problem(&c.id, format!("#{} and #{first} would share the folder {folder} on this computer, so gild left #{}'s history where it was. Rename one of them.", c.name, c.name));
                continue;
            }
            seen.insert(identity(&folder), c.name.clone());
            placed.push((c, folder, new_row));
        }
        stop_here("placed")?;
        for (c, folder, new_row) in placed {
            if let Err(e) = self.move_channel(c, &root, &folder, new_row || moving) {
                self.add_problem(&c.id, e);
            }
        }
        self.write_manifest(workspace_id)
    }

    /// Move one channel's old history into its folder, checked before the old
    /// one is put away. `may_start` lets a channel that never had a history
    /// get an empty repository (only while moving from the old layout).
    fn move_channel(&self, c: &Channel, root: &Path, folder: &str, may_start: bool) -> Result<(), String> {
        let dir = safe_dir(root, folder, &c.id)?;
        if let Some(from) = self.legacy_repo(&c.id) {
            // Only into a folder that's empty, or this channel's own move that
            // stopped halfway (marked with its id before anything was copied).
            let git = dir.join(".git");
            if git.exists() && records::mark_of(&dir).as_deref() != Some(c.id.as_str()) {
                return Err(format!("{} already has a git repository, so gild left #{}'s history where it was ({}).", dir.display(), c.name, from.display()));
            }
            if folder != MAIN && !git.exists() && std::fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_some()) {
                return Err(format!("{} isn't empty, so gild left #{}'s history where it was ({}).", dir.display(), c.name, from.display()));
            }
            let fail = |e: String| format!("Moving #{}'s history into {} didn't work: {e}. Its history is kept as it was, in {}.", c.name, dir.display(), from.display());
            records::mark(&dir, &c.id).map_err(&fail)?;
            records::copy_repo(&from, &dir).map_err(&fail)?;
            stop_here("copied").map_err(&fail)?;
            records::same_history(&from, &dir, folder == MAIN).map_err(&fail)?;
            stop_here("checked").map_err(&fail)?;
            let kept = self.data.join(MIGRATED);
            std::fs::create_dir_all(&kept).map_err(|e| fail(e.to_string()))?;
            std::fs::rename(&from, kept.join(format!("{}.git", c.id))).map_err(|e| fail(e.to_string()))?;
            stop_here("renamed")?;
        } else if !records::is_repo(&dir) && may_start {
            records::init(&dir)?;
            records::mark(&dir, &c.id)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::{Acl, View};
    use crate::store::DEMO_NAME;
    use std::fs;

    fn temp(tag: u32) -> PathBuf {
        let base = std::env::temp_dir().join(format!("gild-folders-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        // Data folders for one gild, or two ("a" and "b").
        for d in ["data", "a", "b"] {
            fs::create_dir_all(base.join(d)).unwrap();
        }
        base
    }

    /// `git status --porcelain` through the git binary, when there is one:
    /// a second opinion from outside gitoxide. None: no git here.
    fn git_status(dir: &Path) -> Option<String> {
        let out = std::process::Command::new("git")
            .args(["-c", "credential.helper=", "-c", "core.fsmonitor=false", "status", "--porcelain", "--untracked-files=all"])
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .ok()?;
        assert!(out.status.success(), "git status failed in {}: {}", dir.display(), String::from_utf8_lossy(&out.stderr));
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// The folder is exactly HEAD: through gitoxide, and through git if it's here.
    fn assert_clean(dir: &Path, main: bool, what: &str) {
        assert_eq!(records::status(dir, main).unwrap(), Vec::<String>::new(), "{what}: {}", dir.display());
        if let Some(s) = git_status(dir) {
            assert_eq!(s, "", "{what}: git status in {}", dir.display());
        }
    }

    fn file(dir: &Path, path: &str) -> String {
        fs::read_to_string(join(dir, path)).unwrap_or_else(|e| panic!("{path} in {}: {e}", dir.display()))
    }

    #[test]
    fn a_workspace_is_a_folder_of_repositories() {
        let base = temp(line!());
        let store = Store::open(&base.join("data")).unwrap();
        let ws = store.create_workspace("Front Desk").unwrap();
        let general = store.channels(&ws.id).unwrap()[0].clone();
        let intake = store.create_channel(&ws.id, "student-intake", "Students").unwrap();
        let root = store.workspace_folder(&ws.id).unwrap();
        assert_eq!(root, base.join("data/Gild/Front Desk"));
        assert_eq!(store.repo(&general.id).unwrap(), root, "#general's repository is the workspace's folder");
        assert_eq!(store.repo(&intake.id).unwrap(), root.join("channels/student-intake"));
        assert!(root.join(".git").is_dir() && root.join("channels/student-intake/.git").is_dir());
        // gild.json lists the channels; the main repository ignores channels/.
        let wf: WorkspaceFile = serde_json::from_str(&file(&root, "gild.json")).unwrap();
        assert_eq!((wf.id.as_str(), wf.name.as_str()), (ws.id.as_str(), "Front Desk"));
        assert!(wf.org.is_some());
        let listed: Vec<(&str, &str, &str)> = wf.channels.iter().map(|c| (c.id.as_str(), c.name.as_str(), c.folder.as_str())).collect();
        assert_eq!(listed, [(general.id.as_str(), "general", "."), (intake.id.as_str(), "student-intake", "channels/student-intake")]);
        assert!(file(&root, "gild.json").contains("\"remote\": null"));
        assert_eq!(file(&root, ".gitignore"), "channels/\n");
        // A second workspace with the same name gets "<name> 2"; one can go elsewhere.
        let again = store.create_workspace("Front Desk").unwrap();
        assert_eq!(store.workspace_folder(&again.id).unwrap(), base.join("data/Gild/Front Desk 2"));
        let there = store.create_workspace_in("Elsewhere", Some(&base.join("picked"))).unwrap();
        assert_eq!(store.workspace_folder(&there.id).unwrap(), base.join("picked/Elsewhere"));
        assert_clean(&root, true, "a new workspace");
        assert_clean(&root.join("channels/student-intake"), false, "a new channel");
        drop(store);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn every_write_leaves_the_folder_exactly_as_its_last_commit() {
        let base = temp(line!());
        let store = Store::open(&base.join("data")).unwrap();
        let ws = store.create_workspace("W").unwrap();
        let general = store.channels(&ws.id).unwrap()[0].clone();
        let intake = store.create_channel(&ws.id, "intake", "").unwrap();
        let dir = store.repo(&intake.id).unwrap();
        let (agent, _) = store.create_agent("helper").unwrap();
        let check = |what: &str| {
            assert_clean(&dir, false, what);
            assert_clean(&store.repo(&general.id).unwrap(), true, what);
        };
        store.set_view(&intake.id, DEMO_NAME, &["Sent by".into()], &["New".into(), "Done".into()]).unwrap();
        check("a view");
        let view: View = serde_json::from_str(&file(&dir, ".gild/view.json")).unwrap();
        assert_eq!(view.statuses, ["New", "Done"]);
        let r = store.add_record(&intake.id, DEMO_NAME, "Jordan", "New", Default::default()).unwrap();
        check("a record");
        store.update_record(&intake.id, DEMO_NAME, &r.id, "Jordan", "Done", Default::default()).unwrap();
        check("a record changed");
        assert!(file(&dir, &format!(".gild/records/{}.json", r.id)).contains("\"Done\""));
        let doc = store.create_doc(&intake.id, DEMO_NAME, "SOP").unwrap();
        store.save_doc(&intake.id, DEMO_NAME, &doc.path, "# SOP\n\nBlue.\n").unwrap();
        check("a doc");
        assert_eq!(file(&dir, "docs/sop.md"), "# SOP\n\nBlue.\n");
        store.set_access(&intake.id, DEMO_NAME, &agent.id, Some(records::Access::Write)).unwrap();
        check("access");
        let acl: Acl = serde_json::from_str(&file(&dir, ".gild/acl.json")).unwrap();
        assert_eq!(acl.agents.get(&agent.id), Some(&records::Access::Write));
        let p = store.propose_doc(&intake.id, "helper", Some(&doc.path), None, "# SOP\n\nRed.\n", "Red").unwrap();
        check("a proposal (a branch: the files don't change)");
        assert_eq!(file(&dir, "docs/sop.md"), "# SOP\n\nBlue.\n");
        store.approve_proposal(&intake.id, DEMO_NAME, &p.id).unwrap();
        check("an approval");
        assert_eq!(file(&dir, "docs/sop.md"), "# SOP\n\nRed.\n");
        let q = store.propose_doc(&intake.id, "helper", Some(&doc.path), None, "# SOP\n\nGreen.\n", "Green").unwrap();
        store.reject_proposal(&intake.id, DEMO_NAME, &q.id).unwrap();
        check("a rejection");
        assert_eq!(file(&dir, "docs/sop.md"), "# SOP\n\nRed.\n");
        store.set_rules(&general.id, DEMO_NAME, false, &[]).unwrap();
        check("org rules in #general");
        let view = store.drive(&intake.id, &crate::view::Actor::Person(DEMO_NAME.into()), &crate::view::Nav::Open(doc.path.clone())).unwrap();
        assert!(view.operations.iter().any(|o| o == "append"), "{:?}", view.operations);
        store.edit_file(&intake.id, &crate::view::Actor::Person(DEMO_NAME.into()), &doc.path, "append", &serde_json::json!({ "text": "More." })).unwrap();
        check("an add-on's edit");
        assert!(file(&dir, "docs/sop.md").contains("More."));
        drop(store);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn files_changed_outside_gild_are_committed_as_the_person() {
        let base = temp(line!());
        let data = base.join("data");
        let store = Store::open(&data).unwrap();
        let ws = store.create_workspace("W").unwrap();
        let general = store.channels(&ws.id).unwrap()[0].clone();
        let intake = store.create_channel(&ws.id, "intake", "").unwrap();
        let dir = store.repo(&intake.id).unwrap();
        let doc = store.create_doc(&intake.id, DEMO_NAME, "SOP").unwrap();
        // Another editor: a doc changed, a doc added, a file dropped in.
        fs::write(dir.join("docs/sop.md"), "# SOP\n\nEdited elsewhere.\n").unwrap();
        fs::write(dir.join("docs/new.md"), "# New one\n").unwrap();
        fs::write(dir.join("notes.txt"), "hello").unwrap();
        assert!(store.check_outside(&intake.id).unwrap());
        assert_eq!(store.doc(&intake.id, &doc.path).unwrap().body, "# SOP\n\nEdited elsewhere.\n", "the index has it");
        assert_eq!(store.doc(&intake.id, "docs/new.md").unwrap().title, "New one");
        assert!(store.files(&intake.id).unwrap().iter().any(|f| f.path == "notes.txt"));
        let log = store.doc_history(&intake.id, "docs/new.md").unwrap();
        assert_eq!((log[0].author.as_str(), log[0].message.as_str()), (DEMO_NAME, "Edit outside gild: docs/new.md, docs/sop.md, notes.txt"));
        assert_clean(&dir, false, "after taking the edits in");
        assert!(!store.check_outside(&intake.id).unwrap(), "nothing more to take in");

        // A broken gild file is left as it is, said plainly, and nothing crashes.
        fs::write(dir.join(".gild/view.json"), "{ not json").unwrap();
        fs::write(dir.join("docs/sop.md"), "# SOP\n\nAgain.\n").unwrap();
        assert!(store.check_outside(&intake.id).unwrap());
        let problems = store.folder_problems(&intake.id);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].starts_with("In #intake's folder: .gild/view.json isn't valid"), "{}", problems[0]);
        assert_eq!(store.doc(&intake.id, &doc.path).unwrap().body, "# SOP\n\nAgain.\n", "the rest went in");
        assert_eq!(records::status(&dir, false).unwrap(), ["Added: .gild/view.json"], "the broken file stays uncommitted");
        assert!(store.channel_data(&intake.id).unwrap().view.is_none());
        fs::write(dir.join(".gild/view.json"), "{\"kind\":\"table\",\"fields\":[],\"statuses\":[\"New\"]}").unwrap();
        assert!(store.check_outside(&intake.id).unwrap());
        assert!(store.folder_problems(&intake.id).is_empty());
        assert_eq!(store.channel_data(&intake.id).unwrap().view.unwrap().statuses, ["New"]);

        // While gild was closed: the scan at start takes it in. In #general,
        // the other channels' folders aren't its files.
        drop(store);
        let root = dir.parent().unwrap().parent().unwrap().to_path_buf();
        fs::write(root.join("readme.md"), "Top level").unwrap();
        fs::remove_file(dir.join("notes.txt")).unwrap();
        let store = Store::open(&data).unwrap();
        assert!(store.files(&general.id).unwrap().iter().any(|f| f.path == "readme.md"));
        assert!(!store.files(&intake.id).unwrap().iter().any(|f| f.path == "notes.txt"), "a deleted file is deleted");
        assert_clean(&root, true, "#general after the scan");
        assert_clean(&dir, false, "#intake after the scan");
        // A file gild is about to write over is kept first, as its own commit.
        fs::write(dir.join("docs/sop.md"), "# SOP\n\nMine.\n").unwrap();
        store.add_record(&intake.id, DEMO_NAME, "Jordan", "New", Default::default()).unwrap();
        assert_eq!(store.doc_history(&intake.id, &doc.path).unwrap()[0].message, "Edit outside gild: docs/sop.md");
        #[cfg(unix)]
        {
            let secret = base.join("secret.txt");
            fs::write(&secret, "private").unwrap();
            std::os::unix::fs::symlink(&secret, dir.join("docs/link.md")).unwrap();
            store.check_outside(&intake.id).unwrap();
            assert!(store.doc(&intake.id, "docs/link.md").is_err(), "a link is never followed into history");
            assert!(store.folder_problems(&intake.id)[0].contains("docs/link.md goes through a link"), "{:?}", store.folder_problems(&intake.id));
        }
        drop(store);
        fs::remove_dir_all(base).unwrap();
    }

    /// A data folder the way gild kept it before 0.24: the index in SQLite,
    /// and each channel's history a bare repository in `channels/<id>.git`,
    /// with a proposal waiting, one approved and one rejected.
    const W1: &str = "000000000001aaaaaa0001";
    const C1: &str = "000000000002aaaaaa0001";
    const C2: &str = "000000000002aaaaaa0002";
    const C3: &str = "000000000002aaaaaa0003";

    fn old_layout(data: &Path) -> (String, Vec<(String, String)>) {
        old_layout_with(data, &[(C1, "general"), (C2, "student-intake"), (C3, "quiet")], 2)
    }

    /// The old layout with these channels; the first `with_history` have one.
    fn old_layout_with(data: &Path, channels: &[(&str, &str)], with_history: usize) -> (String, Vec<(String, String)>) {
        let store = Store::open(data).unwrap();
        store.run("INSERT INTO workspaces (id, name, created_at) VALUES (?1, 'Front Desk', 1)", &[Value::from_text(W1)]).unwrap();
        for (i, (id, name)) in channels.iter().enumerate() {
            store
                .run(
                    "INSERT INTO channels (id, workspace_id, name, topic, created_at) VALUES (?1, ?2, ?3, '', ?4)",
                    &[Value::from_text(id.to_string()), Value::from_text(W1), Value::from_text(name.to_string()), Value::from_i64(i as i64 + 2)],
                )
                .unwrap();
        }
        store.run("INSERT INTO messages (id, channel_id, author, kind, body, created_at) VALUES ('m1', ?1, 'Demo', 'person', 'Morning', 5)", &[Value::from_text(C1)]).unwrap();
        for (id, _) in &channels[..with_history] {
            let bare = data.join("channels").join(format!("{id}.git"));
            fs::create_dir_all(&bare).unwrap();
            gix::init_bare(&bare).unwrap();
            let acl = serde_json::to_vec(&Acl { org: true, ..Acl::default() }).unwrap();
            records::commit(&bare, DEMO_NAME, "Open to everyone", &[(records::ACL_PATH.into(), acl)]).unwrap();
            records::commit(&bare, DEMO_NAME, "Add doc SOP", &[("docs/sop.md".into(), b"# SOP\n\nBlue.\n".to_vec())]).unwrap();
            records::propose(&bare, "p1", "helper", "Red", "docs/sop.md", b"# SOP\n\nRed.\n").unwrap();
            records::approve(&bare, "p1", DEMO_NAME, "Approve Red").unwrap();
            records::propose(&bare, "p2", "helper", "Green", "docs/sop.md", b"# SOP\n\nGreen.\n").unwrap();
            records::reject(&bare, "p2").unwrap();
            records::propose(&bare, "p3", "helper", "Pink", "docs/sop.md", b"# SOP\n\nPink.\n").unwrap();
        }
        let refs = records::refs(&data.join("channels").join(format!("{}.git", channels[0].0))).unwrap();
        assert!(refs.iter().any(|(r, _)| r == "refs/heads/proposals/p3") && refs.iter().any(|(r, _)| r == "refs/gild/rejected/p2"));
        drop(store);
        (W1.into(), refs)
    }

    fn old(data: &Path, id: &str) -> PathBuf {
        data.join("channels").join(format!("{id}.git"))
    }

    /// A channel's history: its refs, and HEAD's tree.
    type History = (Vec<(String, String)>, Option<String>);

    /// What each channel's history is: its refs and HEAD's tree.
    fn histories(data: &Path) -> Vec<History> {
        [C1, C2].iter().map(|c| {
            let bare = old(data, c);
            (records::refs(&bare).unwrap(), records::head_tree(&bare).unwrap())
        }).collect()
    }

    fn assert_moved(data: &Path, before: &[History]) -> PathBuf {
        let store = Store::open(data).unwrap();
        for c in [C1, C2, C3] {
            assert_eq!(store.folder_problems(c), Vec::<String>::new());
        }
        let root = store.workspace_folder(W1).unwrap();
        assert_eq!(root, data.join("Gild/Front Desk"));
        let (main, intake, quiet) = (root.clone(), root.join("channels/student-intake"), root.join("channels/quiet"));
        // #student-intake: every ref (proposals, the rejected one) and HEAD's tree, as they were.
        assert_eq!(records::refs(&intake).unwrap(), before[1].0);
        assert_eq!(records::head_tree(&intake).unwrap(), before[1].1);
        // #general: the same, with gild.json committed on top.
        let main_repo = gix::open(&main).unwrap();
        let head = main_repo.head_commit().unwrap();
        assert_eq!(head.message_raw_sloppy().to_string().trim(), "Describe the workspace in gild.json");
        let was = before[0].0.iter().find(|(r, _)| r == "refs/heads/main" || r == "refs/heads/master").unwrap();
        assert_eq!(head.parent_ids().next().unwrap().to_string(), was.1, "gild.json sits on the old history");
        let now = records::refs(&main).unwrap();
        for (r, to) in &before[0].0 {
            if r != &was.0 {
                assert!(now.contains(&(r.clone(), to.clone())), "{r} kept");
            }
        }
        // Files on disk, clean folders, the index rebuilt, the chat untouched.
        assert_eq!(file(&intake, "docs/sop.md"), "# SOP\n\nRed.\n");
        assert_eq!(file(&main, "docs/sop.md"), "# SOP\n\nRed.\n");
        assert!(file(&main, "gild.json").contains("\"student-intake\""));
        assert!(quiet.join(".git").is_dir(), "a channel with no history gets an empty repository");
        assert_clean(&main, true, "#general moved");
        assert_clean(&intake, false, "#student-intake moved");
        assert_clean(&quiet, false, "#quiet");
        assert_eq!(store.proposals(C2).unwrap().iter().map(|p| p.summary.as_str()).collect::<Vec<_>>(), ["Pink"]);
        assert_eq!(store.doc(C2, "docs/sop.md").unwrap().body, "# SOP\n\nRed.\n");
        assert_eq!(store.messages(C1).unwrap()[0].body, "Morning");
        // The old histories are out of the way but kept, as they were.
        assert!(!data.join("channels").exists(), "{:?}", fs::read_dir(data.join("channels")).map(|d| d.count()));
        for (c, (refs, _)) in [C1, C2].iter().zip(before) {
            assert_eq!(&records::refs(&data.join(MIGRATED).join(format!("{c}.git"))).unwrap(), refs, "the old history is kept in {MIGRATED}");
        }
        drop(store);
        root
    }

    #[test]
    fn old_histories_move_into_the_workspace_folder_once() {
        let base = temp(line!());
        let data = base.join("data");
        let (_, _) = old_layout(&data);
        let before = histories(&data);
        let root = assert_moved(&data, &before);
        // Again: nothing changes, no "Front Desk 2".
        let after = (records::refs(&root).unwrap(), records::refs(&root.join("channels/student-intake")).unwrap());
        let store = Store::open(&data).unwrap();
        assert_eq!(store.workspace_folder(W1).unwrap(), root);
        assert_eq!((records::refs(&root).unwrap(), records::refs(&root.join("channels/student-intake")).unwrap()), after);
        assert!(!data.join("Gild/Front Desk 2").exists());
        drop(store);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_move_that_stopped_halfway_is_finished_next_time() {
        for stop in ["copied", "checked", "renamed"] {
            let base = temp(line!() * 10 + stop.len() as u32);
            let data = base.join("data");
            old_layout(&data);
            let before = histories(&data);
            // Quit after #general's history was copied, after it was checked, or
            // after the old one was put away: the old history is still there
            // until the new one is checked.
            // gild still starts; #general says what happened.
            STOP_AT.with(|s| *s.borrow_mut() = vec![stop]);
            let store = Store::open(&data).unwrap();
            // (Stopped after the old one was put away, the move is done: nothing to say.)
            if stop != "renamed" {
                assert!(store.folder_problems(C1).iter().any(|p| p.contains(&format!("stopped at {stop}"))), "{stop}: {:?}", store.folder_problems(C1));
            }
            assert_eq!(old(&data, C1).exists(), stop != "renamed", "{stop}: the old history is kept where it was until it's checked");
            assert!(store.doc(C2, "docs/sop.md").is_ok(), "{stop}: the other channels moved and work");
            drop(store);
            assert_moved(&data, &before);
            fs::remove_dir_all(base).unwrap();
        }
    }

    #[test]
    fn a_workspace_folder_imports_where_it_is() {
        let base = temp(line!());
        let a = Store::open(&base.join("a")).unwrap();
        let ws = a.create_workspace("Front Desk").unwrap();
        let intake = a.create_channel(&ws.id, "student-intake", "Students").unwrap();
        a.create_channel(&ws.id, "quiet", "").unwrap();
        let doc = a.create_doc(&intake.id, DEMO_NAME, "SOP").unwrap();
        a.save_doc(&intake.id, DEMO_NAME, &doc.path, "# SOP\n\nBlue.\n").unwrap();
        let root = a.workspace_folder(&ws.id).unwrap();
        drop(a);
        // Someone else's computer: the folder, cloned or copied, with one
        // channel's folder missing.
        let clone = base.join("cloned/Front Desk");
        records::copy_dir(&root, &clone).unwrap();
        fs::remove_dir_all(clone.join("channels/quiet")).unwrap();
        let b = Store::open(&base.join("b")).unwrap();
        let back = crate::export::import_workspace(&b, &clone).unwrap();
        assert_eq!((back.id.as_str(), back.name.as_str(), back.channels), (ws.id.as_str(), "Front Desk", 3), "gild.json's ids");
        assert_eq!(b.workspace_folder(&back.id).unwrap(), clone, "opened where it is");
        let chans = b.channels(&back.id).unwrap();
        assert_eq!(chans.iter().map(|c| (c.name.as_str(), c.topic.as_str())).collect::<Vec<_>>(), [("general", "Anything for the whole workspace"), ("student-intake", "Students"), ("quiet", "")]);
        assert_eq!(b.doc(&intake.id, &doc.path).unwrap().body, "# SOP\n\nBlue.\n");
        assert!(clone.join("channels/quiet/.git").is_dir());
        assert_clean(&clone, true, "an opened folder");
        assert_clean(&clone.join("channels/student-intake"), false, "an opened folder's channel");
        // Its org is this computer's now: gild.json says so.
        let wf: WorkspaceFile = serde_json::from_str(&file(&clone, "gild.json")).unwrap();
        assert_eq!(wf.org.as_deref(), Some(b.orgs().unwrap()[0].id.as_str()));
        assert!(crate::export::import_workspace(&b, &clone).unwrap_err().contains("already open in gild, as Front Desk"));
        drop(b);
        fs::remove_dir_all(base).unwrap();
    }

    /// A workspace in `a`, its folder, and its channels: #general and #intake
    /// (with a doc) and #quiet.
    fn workspace(store: &Store) -> (PathBuf, Channel, Channel) {
        let ws = store.create_workspace("Front Desk").unwrap();
        let general = store.channels(&ws.id).unwrap()[0].clone();
        let intake = store.create_channel(&ws.id, "intake", "").unwrap();
        store.create_channel(&ws.id, "quiet", "").unwrap();
        let doc = store.create_doc(&intake.id, DEMO_NAME, "SOP").unwrap();
        store.save_doc(&intake.id, DEMO_NAME, &doc.path, "# SOP\n\nBlue.\n").unwrap();
        (store.workspace_folder(&ws.id).unwrap(), general, intake)
    }

    fn edit_workspace_file(root: &Path, f: impl FnOnce(&mut serde_json::Value)) {
        let mut v: serde_json::Value = serde_json::from_str(&file(root, "gild.json")).unwrap();
        f(&mut v);
        fs::write(root.join("gild.json"), serde_json::to_vec(&v).unwrap()).unwrap();
    }

    #[test]
    fn ids_from_a_file_never_become_paths() {
        let base = temp(line!());
        let a = Store::open(&base.join("a")).unwrap();
        let (root, _, _) = workspace(&a);
        drop(a);
        let b = Store::open(&base.join("b")).unwrap();
        for bad in ["../../a/channels/x", "/etc", "C:\\x", "000000000002AAAAAA0001"] {
            let clone = base.join(format!("clone-{}", bad.len()));
            records::copy_dir(&root, &clone).unwrap();
            edit_workspace_file(&clone, |v| v["channels"][1]["id"] = bad.into());
            let e = crate::export::import_workspace(&b, &clone).unwrap_err();
            assert!(e.contains("isn't one gild makes"), "{bad}: {e}");
            edit_workspace_file(&clone, |v| v["id"] = bad.into());
            assert!(crate::export::import_workspace(&b, &clone).unwrap_err().contains("gild makes"), "{bad}");
        }
        assert!(b.workspaces().unwrap().is_empty(), "nothing was imported");
        drop(b);
        // And a row whose id isn't gild's never names an old history to move:
        // the folder that id would point at is left alone.
        let data = base.join("data");
        old_layout(&data);
        let decoy = data.join("decoy.git");
        fs::create_dir_all(decoy.join("objects")).unwrap();
        fs::write(decoy.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let s = Store::open(&data).unwrap();
        s.run("INSERT INTO channels (id, workspace_id, name, topic, created_at) VALUES ('../decoy', ?1, 'decoy', '', 9)", &[Value::from_text(W1)]).unwrap();
        drop(s);
        Store::open(&data).unwrap();
        assert_eq!(fs::read_to_string(decoy.join("HEAD")).unwrap(), "ref: refs/heads/main\n");
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn links_never_take_gild_out_of_the_folder() {
        use std::os::unix::fs::symlink;
        let base = temp(line!());
        let store = Store::open(&base.join("a")).unwrap();
        let (root, _, intake) = workspace(&store);
        let outside = base.join("outside");
        fs::create_dir_all(&outside).unwrap();
        // A linked docs folder: gild neither writes nor reads through it.
        let dir = root.join("channels/intake");
        fs::rename(dir.join("docs"), base.join("docs-was")).unwrap();
        symlink(&outside, dir.join("docs")).unwrap();
        let e = store.save_doc(&intake.id, DEMO_NAME, "docs/sop.md", "# SOP\n\nOut.\n").unwrap_err();
        assert!(e.contains("link"), "{e}");
        assert!(fs::read_dir(&outside).unwrap().next().is_none(), "nothing was written outside");
        fs::remove_file(dir.join("docs")).unwrap();
        fs::rename(base.join("docs-was"), dir.join("docs")).unwrap();
        // A linked channel folder: the channel isn't used at all.
        fs::rename(&dir, base.join("intake-was")).unwrap();
        symlink(base.join("intake-was"), &dir).unwrap();
        assert!(store.repo(&intake.id).unwrap_err().contains("link"));
        assert!(store.save_doc(&intake.id, DEMO_NAME, "docs/sop.md", "# x\n").is_err());
        drop(store);
        // Opening a folder whose channel is a link out of it: refused, and
        // the folder it points at is never made a repository.
        let clone = base.join("clone");
        fs::remove_file(&dir).unwrap();
        fs::rename(base.join("intake-was"), &dir).unwrap();
        records::copy_dir(&root, &clone).unwrap();
        fs::remove_dir_all(clone.join("channels/quiet")).unwrap();
        symlink(&outside, clone.join("channels/quiet")).unwrap();
        let b = Store::open(&base.join("b")).unwrap();
        assert!(crate::export::import_workspace(&b, &clone).unwrap_err().contains("link"));
        assert!(!outside.join(".git").exists() && b.workspaces().unwrap().is_empty());
        // A linked channels/ folder too.
        fs::remove_file(clone.join("channels/quiet")).unwrap();
        fs::rename(clone.join("channels"), base.join("channels-was")).unwrap();
        symlink(base.join("channels-was"), clone.join("channels")).unwrap();
        assert!(crate::export::import_workspace(&b, &clone).unwrap_err().contains("link"));
        drop(b);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn two_channels_never_share_a_folder() {
        let base = temp(line!());
        let data = base.join("data");
        // σ and ς are one letter to a file system that ignores case.
        old_layout_with(&data, &[(C1, "general"), (C2, "σ-notes"), (C3, "ς-notes")], 3);
        let c3_before = records::refs(&old(&data, C3)).unwrap();
        let store = Store::open(&data).unwrap();
        let root = store.workspace_folder(W1).unwrap();
        assert!(store.folder_problems(C3)[0].contains("would share the folder"), "{:?}", store.folder_problems(C3));
        assert_eq!(records::refs(&old(&data, C3)).unwrap(), c3_before, "its history stays where it was");
        assert_eq!(records::refs(&root.join("channels/σ-notes")).unwrap().len(), c3_before.len(), "the first one moved");
        assert_eq!(records::mark_of(&root.join("channels/σ-notes")).as_deref(), Some(C2));
        drop(store);
        // A folder already holding someone's repository is never written over.
        let base2 = temp(line!());
        let data2 = base2.join("data");
        old_layout(&data2);
        // The folders are decided (a launch that stopped there), then someone
        // puts a repository of their own where #student-intake's would go.
        STOP_AT.with(|s| *s.borrow_mut() = vec!["placed"]);
        drop(Store::open(&data2).unwrap());
        let theirs = data2.join("Gild/Front Desk/channels/student-intake");
        fs::create_dir_all(&theirs).unwrap();
        gix::init(&theirs).unwrap();
        fs::write(theirs.join("mine.txt"), "mine").unwrap();
        let store = Store::open(&data2).unwrap();
        assert!(store.folder_problems(C2).iter().any(|p| p.contains("already has a git repository")), "{:?}", store.folder_problems(C2));
        assert!(old(&data2, C2).exists(), "its history stays where it was");
        assert!(records::refs(&theirs).unwrap().iter().all(|(r, _)| r == "HEAD") && file(&theirs, "mine.txt") == "mine", "theirs untouched");
        assert!(store.doc(C2, "docs/sop.md").is_err() && store.channel(C2).unwrap().is_some());
        // New channels are checked the same way before anything is written.
        let ws = store.create_workspace("Other").unwrap();
        store.create_channel(&ws.id, "σ-x", "").unwrap();
        assert!(store.create_channel(&ws.id, "ς-x", "").unwrap_err().contains("share a folder"));
        drop(store);
        fs::remove_dir_all(base).unwrap();
        fs::remove_dir_all(base2).unwrap();
    }

    #[test]
    fn a_channel_that_fails_to_move_doesnt_stop_gild() {
        let base = temp(line!());
        let data = base.join("data");
        old_layout(&data);
        // #student-intake's old history is damaged: its objects are gone.
        let objects = old(&data, C2).join("objects");
        fs::remove_dir_all(&objects).unwrap();
        fs::create_dir_all(&objects).unwrap();
        let store = Store::open(&data).expect("gild starts");
        assert!(store.folder_problems(C2).iter().any(|p| p.contains("didn't work") && p.contains("kept as it was")), "{:?}", store.folder_problems(C2));
        assert!(old(&data, C2).join("HEAD").is_file(), "the old history is left where it was");
        assert_eq!(store.doc(C1, "docs/sop.md").unwrap().body, "# SOP\n\nRed.\n", "the others moved and work");
        assert!(store.save_doc(C2, DEMO_NAME, "docs/sop.md", "x").is_err(), "the failed one isn't written to");
        drop(store);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_copy_missing_an_object_is_never_taken_for_the_history() {
        let base = temp(line!());
        let data = base.join("data");
        old_layout(&data);
        let (from, copy) = (old(&data, C2), base.join("copy"));
        records::copy_repo(&from, &copy).unwrap();
        records::same_history(&from, &copy, false).unwrap();
        // Lose the blob of the first version of the doc (only old commits reach it).
        let repo = gix::open(&from).unwrap();
        let blue = gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, b"# SOP\n\nBlue.\n").unwrap().to_string();
        fs::remove_file(copy.join(".git/objects").join(&blue[..2]).join(&blue[2..])).unwrap();
        let e = records::same_history(&from, &copy, false).unwrap_err();
        assert!(e.contains("incomplete"), "{e}");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_channel_that_cant_be_made_leaves_nothing_behind() {
        let base = temp(line!());
        let data = base.join("data");
        let store = Store::open(&data).unwrap();
        let ws = store.create_workspace("W").unwrap();
        let root = store.workspace_folder(&ws.id).unwrap();
        fs::create_dir_all(root.join("channels/intake")).unwrap();
        fs::write(root.join("channels/intake/notes.txt"), "mine").unwrap();
        assert!(store.create_channel(&ws.id, "intake", "").unwrap_err().contains("already a channels/intake"));
        assert_eq!(store.channels(&ws.id).unwrap().len(), 1, "no channel row");
        assert!(store.query("SELECT 1 FROM channel_folders WHERE folder = 'channels/intake'", &[]).unwrap().is_empty());
        drop(store);
        let store = Store::open(&data).unwrap();
        assert!(!root.join("channels/intake/.git").exists(), "the folder never became a repository");
        assert_eq!(file(&root, "channels/intake/notes.txt"), "mine");
        drop(store);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_missing_channel_folder_is_a_problem_not_a_fresh_start() {
        let base = temp(line!());
        let data = base.join("a");
        let store = Store::open(&data).unwrap();
        let (root, _, intake) = workspace(&store);
        drop(store);
        let dir = root.join("channels/intake");
        fs::rename(&dir, base.join("away")).unwrap();
        let store = Store::open(&data).unwrap();
        assert!(store.folder_problems(&intake.id).iter().any(|p| p.contains("is gone. Put it back")), "{:?}", store.folder_problems(&intake.id));
        assert_eq!(store.doc(&intake.id, "docs/sop.md").unwrap().body, "# SOP\n\nBlue.\n", "the index is kept");
        assert!(store.save_doc(&intake.id, DEMO_NAME, "docs/sop.md", "# x\n").is_err());
        assert!(!dir.exists(), "never made again empty");
        drop(store);
        fs::rename(base.join("away"), &dir).unwrap();
        let store = Store::open(&data).unwrap();
        assert!(store.folder_problems(&intake.id).is_empty());
        store.save_doc(&intake.id, DEMO_NAME, "docs/sop.md", "# SOP\n\nBack.\n").unwrap();
        drop(store);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn gild_never_writes_over_a_save_it_hasnt_seen() {
        let base = temp(line!());
        let store = Store::open(&base.join("a")).unwrap();
        let (root, _, intake) = workspace(&store);
        let dir = root.join("channels/intake");
        // Saved by another app after gild looked, before gild writes.
        fs::write(dir.join("docs/sop.md"), "# SOP\n\nTheirs.\n").unwrap();
        let e = records::commit(&dir, DEMO_NAME, "Edit SOP", &[("docs/sop.md".into(), b"# SOP\n\nOurs.\n".to_vec())]).unwrap_err();
        assert!(e.contains("changed outside gild just now"), "{e}");
        assert_eq!(file(&dir, "docs/sop.md"), "# SOP\n\nTheirs.\n");
        // Through the store, theirs goes in first, as its own commit.
        store.save_doc(&intake.id, DEMO_NAME, "docs/sop.md", "# SOP\n\nOurs.\n").unwrap();
        let log: Vec<String> = store.doc_history(&intake.id, "docs/sop.md").unwrap().into_iter().map(|c| c.message).collect();
        assert_eq!(&log[..2], ["Edit SOP", "Edit outside gild: docs/sop.md"]);
        drop(store);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_huge_file_isnt_taken_in() {
        let base = temp(line!());
        let store = Store::open(&base.join("a")).unwrap();
        let (root, _, intake) = workspace(&store);
        let big = fs::File::create(root.join("channels/intake/video.mov")).unwrap();
        big.set_len(records::MAX_OUTSIDE_FILE + 1).unwrap();
        drop(big);
        store.check_outside(&intake.id).unwrap();
        assert!(store.folder_problems(&intake.id).iter().any(|p| p.contains("video.mov is bigger than 50 MB")), "{:?}", store.folder_problems(&intake.id));
        assert!(!store.files(&intake.id).unwrap().iter().any(|f| f.path == "video.mov"));
        drop(store);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn gild_commits_to_its_branch_whatever_is_checked_out() {
        let base = temp(line!());
        let store = Store::open(&base.join("a")).unwrap();
        let (root, _, intake) = workspace(&store);
        let dir = root.join("channels/intake");
        let branch = records::branch_of(&dir).unwrap();
        let tip = records::refs(&dir).unwrap().into_iter().find(|(r, _)| *r == branch).unwrap().1;
        // Someone checks out another branch in the folder.
        fs::write(dir.join(".git/HEAD"), "ref: refs/heads/experiment\n").unwrap();
        let e = store.save_doc(&intake.id, DEMO_NAME, "docs/sop.md", "# SOP\n\nRed.\n").unwrap_err();
        assert!(e.contains("Switch back"), "{e}");
        store.check_outside(&intake.id).unwrap();
        assert!(store.folder_problems(&intake.id)[0].contains("Switch back"));
        assert_eq!(records::refs(&dir).unwrap().into_iter().find(|(r, _)| *r == branch).unwrap().1, tip, "gild's branch didn't move");
        assert!(!records::refs(&dir).unwrap().iter().any(|(r, _)| r == "refs/heads/experiment"), "and nothing went to theirs");
        // Back on it: gild carries on, on its branch; gild.json names it.
        fs::write(dir.join(".git/HEAD"), format!("ref: {branch}\n")).unwrap();
        store.save_doc(&intake.id, DEMO_NAME, "docs/sop.md", "# SOP\n\nRed.\n").unwrap();
        assert!(file(&root, "gild.json").contains(&format!("\"branch\": \"{branch}\"")));
        drop(store);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_bad_import_leaves_nothing_behind() {
        let base = temp(line!());
        let a = Store::open(&base.join("a")).unwrap();
        let (root, _, _) = workspace(&a);
        let ws = a.workspaces().unwrap()[0].id.clone();
        let out = crate::export::export_workspace(&a, &ws, &base.join("out"), "x").unwrap();
        drop(a);
        let b = Store::open(&base.join("b")).unwrap();
        // Two channels with one id: refused before anything is written.
        let clone = base.join("clone");
        records::copy_dir(&root, &clone).unwrap();
        edit_workspace_file(&clone, |v| v["channels"][2]["id"] = v["channels"][1]["id"].clone());
        assert!(crate::export::import_workspace(&b, &clone).unwrap_err().contains("Two channels have the id"));
        // A history that fails while copying (the last channel's, damaged):
        // the channels already written are forgotten too.
        let objects = out.join("workspace/channels/quiet/.git/objects");
        fs::remove_dir_all(&objects).unwrap();
        fs::create_dir_all(&objects).unwrap();
        assert!(crate::export::import_workspace(&b, &out).is_err());
        assert!(b.workspaces().unwrap().is_empty(), "no half-imported workspace");
        assert!(b.query("SELECT 1 FROM channels", &[]).unwrap().is_empty() && b.query("SELECT 1 FROM messages", &[]).unwrap().is_empty());
        drop(b);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn folder_names() {
        assert!(reserved("COM¹") && reserved("lpt³") && reserved("COM1") && reserved("nul.txt") && !reserved("COM10") && !reserved("console"));
        assert_eq!(folder_name("Front Desk"), "Front Desk");
        assert_eq!(folder_name("../../etc"), "etc");
        assert_eq!(folder_name("CON"), "CON workspace");
        assert_eq!(folder_name(""), "untitled");
        assert_eq!(channel_folder("nul"), "channels/_nul");
        assert_eq!(channel_folder("intake"), "channels/intake");
        assert_eq!(join(Path::new("/w"), "."), PathBuf::from("/w"));
        assert_eq!(join(Path::new("/w"), "channels/x"), Path::new("/w").join("channels").join("x"));
    }
}
