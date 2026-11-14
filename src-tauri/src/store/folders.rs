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
}

/// Names Windows keeps for devices: never a file or folder name.
fn reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit())
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

    /// A channel's repository: its folder in its workspace's.
    pub(crate) fn repo(&self, channel_id: &str) -> Result<PathBuf, String> {
        let row = self.query(
            "SELECT w.path, f.folder FROM channels c JOIN workspace_folders w ON w.workspace_id = c.workspace_id
             JOIN channel_folders f ON f.channel_id = c.id WHERE c.id = ?1",
            &[Value::from_text(channel_id.to_string())],
        )?;
        let row = row.first().ok_or("That channel no longer exists.")?;
        let root = PathBuf::from(text(&row[0]));
        if !root.is_dir() {
            return Err(format!("This workspace's folder, {}, is gone. Put it back to use the workspace again.", root.display()));
        }
        Ok(join(&root, &text(&row[1])))
    }

    fn is_main(&self, channel_id: &str) -> Result<bool, String> {
        Ok(self.folder_of(channel_id)?.as_deref() == Some(MAIN))
    }

    /// Give a new channel its folder and repository.
    pub(super) fn place_channel(&self, channel: &Channel) -> Result<(), String> {
        let root = self.workspace_folder(&channel.workspace_id)?;
        let has_main = self
            .channels(&channel.workspace_id)?
            .iter()
            .any(|c| c.id != channel.id && self.folder_of(&c.id).ok().flatten().as_deref() == Some(MAIN));
        let folder = if has_main { channel_folder(&channel.name) } else { MAIN.to_string() };
        let dir = join(&root, &folder);
        if folder != MAIN && dir.exists() {
            return Err(format!("There's already a folder {} in the workspace's folder: move it away to make #{} there.", folder, channel.name));
        }
        self.set_channel_folder(&channel.id, &folder)?;
        records::init(&dir)
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
            channels.push(ChannelEntry { id: c.id, name: c.name, topic: c.topic, folder, remote: None });
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
                self.set_problems(&id, vec![e]);
            }
        }
        Ok(())
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
    /// (`<data>/channels/<id>.git`) into it. Safe to run again: a move that
    /// stopped halfway is finished, and a finished one is left alone.
    pub(super) fn ensure_folders(&self) -> Result<(), String> {
        let legacy = self.data.join("channels");
        for ws in self.workspaces()? {
            let channels = self.channels(&ws.id)?;
            let old = |c: &Channel| legacy.join(format!("{}.git", c.id));
            let root = match self.workspace_root(&ws.id)? {
                Some(root) => {
                    if !root.is_dir() && !channels.iter().any(|c| old(c).exists()) {
                        // Moved or deleted by someone: never start it again empty.
                        let gone = format!("This workspace's folder, {}, is gone. Put it back to use the workspace again.", root.display());
                        for c in &channels {
                            self.set_problems(&c.id, vec![gone.clone()]);
                        }
                        continue;
                    }
                    root
                }
                None => {
                    let root = self.free_folder(&self.home, &ws.name)?;
                    self.set_workspace_folder(&ws.id, &root)?;
                    root
                }
            };
            std::fs::create_dir_all(&root).map_err(|e| err(&format!("creating the folder {}", root.display()), e))?;
            let main = main_index(channels.iter().map(|c| c.name.as_str()));
            for (i, c) in channels.iter().enumerate() {
                let folder = match self.folder_of(&c.id)? {
                    Some(f) => f,
                    None => {
                        let f = if i == main { MAIN.to_string() } else { channel_folder(&c.name) };
                        self.set_channel_folder(&c.id, &f)?;
                        f
                    }
                };
                let dir = join(&root, &folder);
                let from = old(c);
                let done = legacy.join(format!("{}.git.moved", c.id));
                if from.exists() {
                    records::copy_repo(&from, &dir).map_err(|e| format!("Moving #{}'s history into {}: {e}", c.name, dir.display()))?;
                    stop_here("copied")?;
                    records::same_history(&from, &dir, folder == MAIN).map_err(|e| {
                        format!("Moving #{}'s history into {} didn't work: {e}. The old history is kept in {}.", c.name, dir.display(), from.display())
                    })?;
                    stop_here("checked")?;
                    std::fs::rename(&from, &done).map_err(|e| err("putting the old history away", e))?;
                    stop_here("renamed")?;
                }
                if done.exists() {
                    std::fs::remove_dir_all(&done).map_err(|e| err("deleting the old history", e))?;
                }
                if !records::is_repo(&dir) {
                    records::init(&dir)?;
                }
            }
            self.write_manifest(&ws.id)?;
        }
        if std::fs::read_dir(&legacy).map(|mut d| d.next().is_none()).unwrap_or(false) {
            std::fs::remove_dir(&legacy).map_err(|e| err("deleting the old history folder", e))?;
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
            assert!(store.folder_problems(&intake.id)[0].contains("docs/link.md is a link"));
        }
        drop(store);
        fs::remove_dir_all(base).unwrap();
    }

    /// A data folder the way gild kept it before 0.24: the index in SQLite,
    /// and each channel's history a bare repository in `channels/<id>.git`,
    /// with a proposal waiting, one approved and one rejected.
    fn old_layout(data: &Path) -> (String, Vec<(String, String)>) {
        let store = Store::open(data).unwrap();
        store.run("INSERT INTO workspaces (id, name, created_at) VALUES ('w1', 'Front Desk', 1)", &[]).unwrap();
        let channels = [("c1", "general"), ("c2", "student-intake"), ("c3", "quiet")];
        for (i, (id, name)) in channels.iter().enumerate() {
            store
                .run(
                    "INSERT INTO channels (id, workspace_id, name, topic, created_at) VALUES (?1, 'w1', ?2, '', ?3)",
                    &[Value::from_text(id.to_string()), Value::from_text(name.to_string()), Value::from_i64(i as i64 + 2)],
                )
                .unwrap();
        }
        store.run("INSERT INTO messages (id, channel_id, author, kind, body, created_at) VALUES ('m1', 'c1', 'Demo', 'person', 'Morning', 5)", &[]).unwrap();
        for (id, _) in &channels[..2] {
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
        let refs = records::refs(&data.join("channels/c2.git")).unwrap();
        assert!(refs.iter().any(|(r, _)| r == "refs/heads/proposals/p3") && refs.iter().any(|(r, _)| r == "refs/gild/rejected/p2"));
        drop(store);
        ("w1".into(), refs)
    }

    /// A channel's history: its refs, and HEAD's tree.
    type History = (Vec<(String, String)>, Option<String>);

    /// What each channel's history is: its refs and HEAD's tree.
    fn histories(data: &Path) -> Vec<History> {
        ["c1", "c2"].iter().map(|c| {
            let bare = data.join("channels").join(format!("{c}.git"));
            (records::refs(&bare).unwrap(), records::head_tree(&bare).unwrap())
        }).collect()
    }

    fn assert_moved(data: &Path, before: &[History]) -> PathBuf {
        let store = Store::open(data).unwrap();
        let root = store.workspace_folder("w1").unwrap();
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
        assert_eq!(store.proposals("c2").unwrap().iter().map(|p| p.summary.as_str()).collect::<Vec<_>>(), ["Pink"]);
        assert_eq!(store.doc("c2", "docs/sop.md").unwrap().body, "# SOP\n\nRed.\n");
        assert_eq!(store.messages("c1").unwrap()[0].body, "Morning");
        // The old histories are gone.
        assert!(!data.join("channels").exists(), "{:?}", fs::read_dir(data.join("channels")).map(|d| d.count()));
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
        assert_eq!(store.workspace_folder("w1").unwrap(), root);
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
            STOP_AT.with(|s| *s.borrow_mut() = vec![stop]);
            assert_eq!(Store::open(&data).err().unwrap(), format!("stopped at {stop}"));
            let old_general = data.join("channels/c1.git");
            assert_eq!(old_general.exists(), stop != "renamed", "{stop}: the old history is kept until it's checked");
            assert!(data.join("channels/c2.git").exists(), "{stop}: the next channel wasn't touched");
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

    #[test]
    fn folder_names() {
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
