//! The Files add-on: every channel's files, as the pane and agents see
//! them. Built into gild (`addons/files/gild-addon.json`): its tools touch
//! the channel's repository, which no sandboxed module may.
//!
//! Every write here is one commit by whoever asked, through the same
//! hardened path as records and docs (`records::commit_changes`: paths
//! checked, no links followed, the folder and git's index left matching
//! the commit). What Files never touches: gild's own files (`.gild/`, the
//! workspace's `gild.json`, the main repository's `.gitignore`) and `.git`.
//! A folder with nothing else in it is kept by an empty `.gitkeep`, as git
//! keeps no empty folders.

use std::collections::BTreeSet;
use std::path::PathBuf;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use super::folders::identity;
use super::{Store, MAX_OPEN};
use crate::addons::FILES;
use crate::records::{self, Change, FileEntry, PaneSetting, PanesFile, ADDONS_PATH, MAX_OUTSIDE_FILE};

/// The file that keeps an otherwise empty folder in the history.
pub const KEEP: &str = ".gitkeep";
/// The largest image the pane previews, as bytes.
pub const MAX_PREVIEW: usize = 10_000_000;

/// A channel's files and folders, as `list_files` answers.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct FileList {
    pub files: Vec<FileEntry>,
    pub folders: Vec<String>,
}

/// What the pane shows for an open file: its text, an image, or neither.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct FilePreview {
    pub path: String,
    pub size: u64,
    /// `text`, `image` or `other`.
    pub kind: &'static str,
    pub text: Option<String>,
    /// An image as a `data:` URL.
    pub data: Option<String>,
}

/// An add-on's pane in a channel: shown or hidden, in the channel's order.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Pane {
    pub addon: String,
    pub name: String,
    pub hidden: bool,
}

/// A file the window hands in to upload: its name and its bytes, base64.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Upload {
    pub name: String,
    pub data: String,
}

/// gild's own files: never listed as the channel's, never changed through
/// Files (an agent that could write `.gild/acl.json` would grant itself
/// access).
pub fn own_file(path: &str) -> bool {
    path == ".gild" || path.starts_with(".gild/") || path == records::MANIFEST || path == ".gitignore"
}

/// Every folder a list of files implies: each ancestor of each file.
pub fn folders_of(files: &[FileEntry]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for f in files {
        let parts: Vec<&str> = f.path.split('/').collect();
        for n in 1..parts.len() {
            out.insert(parts[..n].join("/"));
        }
    }
    out
}

fn ancestors(path: &str) -> Vec<String> {
    let parts: Vec<&str> = path.split('/').collect();
    (1..parts.len()).map(|n| parts[..n].join("/")).collect()
}

fn image_mime(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        _ => return None,
    })
}

/// The file at `path`, or every file in the folder `path`.
fn under<'a>(files: &'a [FileEntry], path: &str) -> Vec<&'a FileEntry> {
    let prefix = format!("{path}/");
    files.iter().filter(|f| f.path == path || f.path.starts_with(&prefix)).collect()
}

/// Nothing may be at `path` now: not a file, not a folder, not a name this
/// computer's file system wouldn't tell from it (case, Unicode form), and
/// no file where one of its folders would be.
fn free(files: &[FileEntry], path: &str) -> Result<(), String> {
    let folders = folders_of(files);
    if files.iter().any(|f| f.path == path) {
        return Err(format!("There's already a file at {path}."));
    }
    if folders.contains(path) {
        return Err(format!("{path} is a folder."));
    }
    let mut places = ancestors(path);
    places.push(path.to_string());
    for place in places {
        let id = identity(&place);
        if place != path && files.iter().any(|f| f.path == place) {
            return Err(format!("{place} is a file, so nothing can go inside it."));
        }
        if let Some(f) = files.iter().find(|f| f.path != place && identity(&f.path) == id) {
            return Err(format!("{} is already there, and this computer doesn't tell it from {place}. Pick another name.", f.path));
        }
        if let Some(d) = folders.iter().find(|d| **d != place && identity(d) == id) {
            return Err(format!("{d} is already there, and this computer doesn't tell it from {place}. Pick another name."));
        }
    }
    Ok(())
}

/// The order and visibility a channel saved, over gild's list of panes:
/// what it saved first (unknown add-ons dropped), then the rest, shown.
fn resolve(saved: &[PaneSetting], known: &[(&str, &str)]) -> Vec<Pane> {
    let mut out: Vec<Pane> = Vec::new();
    for s in saved {
        if let Some((id, name)) = known.iter().find(|(id, _)| *id == s.addon) {
            if !out.iter().any(|p| p.addon == *id) {
                out.push(Pane { addon: id.to_string(), name: name.to_string(), hidden: s.hidden });
            }
        }
    }
    for (id, name) in known {
        if !out.iter().any(|p| p.addon == *id) {
            out.push(Pane { addon: id.to_string(), name: name.to_string(), hidden: false });
        }
    }
    out
}

impl Store {
    /// A path Files may change: checked like every path, and never gild's own.
    fn file_path_ok(path: &str) -> Result<(), String> {
        records::check_path(path)?;
        if own_file(path) {
            return Err(format!("{path} is one of gild's own files; it isn't changed through Files."));
        }
        Ok(())
    }

    /// The Files add-on edits files only while it has its grant.
    fn files_allowed(&self) -> Result<(), String> {
        if !self.addon_granted(FILES, "files", "*")? {
            return Err("The Files add-on isn't allowed to change files (Settings › Add-ons › Files › Permissions).".into());
        }
        Ok(())
    }

    /// Every file and folder in a channel, gild's own and `.gitkeep` aside.
    pub fn list_files(&self, channel_id: &str) -> Result<FileList, String> {
        let all = self.files(channel_id)?;
        let folders = folders_of(&all).into_iter().collect();
        let files = all.into_iter().filter(|f| f.path.rsplit('/').next() != Some(KEEP) && !own_file(&f.path)).collect();
        Ok(FileList { files, folders })
    }

    /// What the pane shows for a file: text (up to MAX_OPEN), an image (up
    /// to MAX_PREVIEW, as a data: URL), or only its name and size.
    pub fn file_preview(&self, channel_id: &str, path: &str) -> Result<FilePreview, String> {
        self.require_channel(channel_id)?;
        records::check_path(path)?;
        let bytes = records::read_file(&self.repo(channel_id)?, path)?.ok_or_else(|| format!("There's no file {path} in this channel."))?;
        let size = bytes.len() as u64;
        let mut preview = FilePreview { path: path.to_string(), size, kind: "other", text: None, data: None };
        if let Some(mime) = image_mime(path) {
            if bytes.len() <= MAX_PREVIEW {
                preview.kind = "image";
                preview.data = Some(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes)));
            }
        } else if bytes.len() <= MAX_OPEN {
            if let Ok(text) = String::from_utf8(bytes) {
                preview.kind = "text";
                preview.text = Some(text);
            }
        }
        Ok(preview)
    }

    /// Write a file: new, or over the one there. The same bytes change
    /// nothing. One commit by `author`.
    pub fn write_file(&self, channel_id: &str, author: &str, path: &str, bytes: Vec<u8>) -> Result<FileEntry, String> {
        self.require_channel(channel_id)?;
        Self::file_path_ok(path)?;
        self.files_allowed()?;
        if bytes.len() as u64 > MAX_OUTSIDE_FILE {
            return Err(format!("Files are at most {} MB.", MAX_OUTSIDE_FILE / 1_000_000));
        }
        let files = self.files(channel_id)?;
        let entry = FileEntry { path: path.to_string(), size: bytes.len() as u64 };
        if files.iter().any(|f| f.path == path) {
            if records::read_file(&self.repo(channel_id)?, path)?.as_deref() == Some(bytes.as_slice()) {
                return Ok(entry);
            }
            self.write(channel_id, author, &format!("Edit {path}"), &[(path.to_string(), bytes)])?;
        } else {
            free(&files, path)?;
            self.write(channel_id, author, &format!("Add {path}"), &[(path.to_string(), bytes)])?;
        }
        Ok(entry)
    }

    /// Files the window hands in (chosen or dropped), into `folder` ("" for
    /// the top), as one commit by `author`. Nothing may be at their paths.
    pub fn upload_files(&self, channel_id: &str, author: &str, folder: &str, uploads: &[Upload]) -> Result<Vec<FileEntry>, String> {
        self.require_channel(channel_id)?;
        self.files_allowed()?;
        if uploads.is_empty() {
            return Err("Nothing to upload.".into());
        }
        let files = self.files(channel_id)?;
        let mut changes = Vec::new();
        for u in uploads {
            if u.name.is_empty() || u.name.contains('/') {
                return Err(format!("{} isn't a file name.", u.name));
            }
            let path = if folder.is_empty() { u.name.clone() } else { format!("{folder}/{}", u.name) };
            Self::file_path_ok(&path)?;
            let bytes = base64::engine::general_purpose::STANDARD.decode(&u.data).map_err(|_| format!("{} didn't arrive whole. Try again.", u.name))?;
            if bytes.len() as u64 > MAX_OUTSIDE_FILE {
                return Err(format!("{} is bigger than {} MB, the most a file in a channel can be.", u.name, MAX_OUTSIDE_FILE / 1_000_000));
            }
            free(&files, &path)?;
            if changes.iter().any(|(p, _): &(String, Vec<u8>)| identity(p) == identity(&path)) {
                return Err(format!("{} is in the upload twice.", u.name));
            }
            changes.push((path, bytes));
        }
        let named: Vec<&str> = changes.iter().take(3).map(|(p, _)| p.as_str()).collect();
        let more = changes.len().saturating_sub(named.len());
        let message = format!("Add {}{}", named.join(", "), if more > 0 { format!(" and {more} more") } else { String::new() });
        self.write(channel_id, author, &message, &changes)?;
        Ok(changes.into_iter().map(|(path, bytes)| FileEntry { path, size: bytes.len() as u64 }).collect())
    }

    /// A new, empty folder (kept in the history by a `.gitkeep`).
    pub fn new_folder(&self, channel_id: &str, author: &str, path: &str) -> Result<String, String> {
        self.require_channel(channel_id)?;
        Self::file_path_ok(path)?;
        self.files_allowed()?;
        free(&self.files(channel_id)?, path)?;
        self.write(channel_id, author, &format!("New folder {path}"), &[(format!("{path}/{KEEP}"), Vec::new())])?;
        Ok(path.to_string())
    }

    /// Move or rename a file, or a folder and everything in it. Nothing may
    /// be at the new path. One commit by `author`.
    pub fn move_file(&self, channel_id: &str, author: &str, from: &str, to: &str) -> Result<(), String> {
        self.require_channel(channel_id)?;
        Self::file_path_ok(from)?;
        Self::file_path_ok(to)?;
        self.files_allowed()?;
        if from == to {
            return Err(format!("{from} is already there."));
        }
        if to.starts_with(&format!("{from}/")) {
            return Err(format!("{from} can't move inside itself."));
        }
        let files = self.files(channel_id)?;
        let moving = under(&files, from);
        if moving.is_empty() {
            return Err(format!("There's nothing at {from} in this channel."));
        }
        let rest: Vec<FileEntry> = files.iter().filter(|f| !moving.iter().any(|m| m.path == f.path)).cloned().collect();
        free(&rest, to)?;
        let repo = self.repo(channel_id)?;
        let mut changes = Vec::new();
        for f in &moving {
            let new = if f.path == from { to.to_string() } else { format!("{to}/{}", &f.path[from.len() + 1..]) };
            // A name that differs only in case or form is the same file to
            // this computer, so it can't be removed and written in one step.
            if new != f.path && identity(&new) == identity(&f.path) {
                return Err(format!("This computer doesn't tell {} from {new}. Move it to another name first, then to {new}.", f.path));
            }
            let bytes = records::read_file(&repo, &f.path)?.ok_or_else(|| format!("{} isn't in the channel's history.", f.path))?;
            changes.push((f.path.clone(), None));
            changes.push((new, Some(bytes)));
        }
        self.write_changes(channel_id, author, &format!("Move {from} to {to}"), &changes)
    }

    /// Delete a file, or a folder and everything in it. The history keeps it.
    pub fn delete_file(&self, channel_id: &str, author: &str, path: &str) -> Result<(), String> {
        self.require_channel(channel_id)?;
        Self::file_path_ok(path)?;
        self.files_allowed()?;
        let files = self.files(channel_id)?;
        let going = under(&files, path);
        if going.is_empty() {
            return Err(format!("There's nothing at {path} in this channel."));
        }
        let changes: Vec<(String, Option<Vec<u8>>)> = going.iter().map(|f| (f.path.clone(), None)).collect();
        self.write_changes(channel_id, author, &format!("Delete {path}"), &changes)
    }

    /// The commits that changed a file, or anything in a folder, newest first.
    pub fn file_history(&self, channel_id: &str, path: &str) -> Result<Vec<Change>, String> {
        self.require_channel(channel_id)?;
        records::check_path(path)?;
        records::history(&self.repo(channel_id)?, path)
    }

    /// Where a file is on disk, to show it in the file manager.
    pub fn file_location(&self, channel_id: &str, path: &str) -> Result<PathBuf, String> {
        self.require_channel(channel_id)?;
        records::file_on_disk(&self.repo(channel_id)?, path)
    }

    /// The channel's add-on panes, in its order, each shown or hidden.
    pub fn channel_panes(&self, channel_id: &str) -> Result<Vec<Pane>, String> {
        self.require_channel(channel_id)?;
        let saved: PanesFile = match records::read_file(&self.repo(channel_id)?, ADDONS_PATH)? {
            Some(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("reading {ADDONS_PATH}: {e}"))?,
            None => PanesFile::default(),
        };
        Ok(resolve(&saved.panes, &self.addons.panes()))
    }

    /// Set the channel's panes: their order, and which are hidden. A pane
    /// left out stays, shown, after the ones given. One commit by `author`.
    pub fn set_channel_panes(&self, channel_id: &str, author: &str, panes: &[PaneSetting]) -> Result<Vec<Pane>, String> {
        let known = self.addons.panes();
        for (i, p) in panes.iter().enumerate() {
            if !known.iter().any(|(id, _)| *id == p.addon) {
                return Err(format!("There's no add-on pane called {}.", p.addon));
            }
            if panes[..i].iter().any(|q| q.addon == p.addon) {
                return Err(format!("{} is listed twice.", p.addon));
            }
        }
        let current = self.channel_panes(channel_id)?;
        let next = resolve(panes, &known);
        if next == current {
            return Ok(current);
        }
        let file = PanesFile { panes: next.iter().map(|p| PaneSetting { addon: p.addon.clone(), hidden: p.hidden }).collect() };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|e| format!("writing {ADDONS_PATH}: {e}"))?;
        let message = format!("Panes: {}", next.iter().map(|p| format!("{}{}", p.name, if p.hidden { " (hidden)" } else { "" })).collect::<Vec<_>>().join(", "));
        self.write(channel_id, author, &message, &[(ADDONS_PATH.to_string(), bytes)])?;
        self.channel_panes(channel_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::DEMO_NAME;
    use crate::view::{Actor, Nav};

    fn store() -> (PathBuf, Store, String) {
        let dir = std::env::temp_dir().join(format!("gild-files-{}", crate::store::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir).unwrap();
        let ws = store.create_workspace("W").unwrap();
        let c = store.create_channel(&ws.id, "intake", "").unwrap();
        (dir, store, c.id)
    }

    fn paths(store: &Store, c: &str) -> Vec<String> {
        store.list_files(c).unwrap().files.into_iter().map(|f| f.path).collect()
    }

    fn git_clean(store: &Store, c: &str) {
        let dir = store.repo(c).unwrap();
        assert_eq!(records::status(&dir, false).unwrap(), Vec::<String>::new(), "the folder and index match the commit");
    }

    #[test]
    fn files_are_written_moved_and_deleted_as_commits_by_the_caller() {
        let (dir, store, c) = store();
        let me = DEMO_NAME;
        // A new file, in folders that didn't exist; the same text again is no commit.
        let f = store.write_file(&c, me, "forms/consent.txt", b"Sign here.".to_vec()).unwrap();
        assert_eq!((f.path.as_str(), f.size), ("forms/consent.txt", 10));
        store.write_file(&c, "ava", "forms/consent.txt", b"Sign here.".to_vec()).unwrap();
        store.write_file(&c, "ava", "forms/consent.txt", b"Sign here, twice.".to_vec()).unwrap();
        let log: Vec<String> = store.file_history(&c, "forms/consent.txt").unwrap().iter().map(|h| format!("{}: {}", h.author, h.message)).collect();
        assert_eq!(log, ["ava: Edit forms/consent.txt", "Demo: Add forms/consent.txt"]);
        let repo = store.repo(&c).unwrap();
        assert_eq!(std::fs::read_to_string(repo.join("forms/consent.txt")).unwrap(), "Sign here, twice.");
        git_clean(&store, &c);

        // A folder of its own, kept by .gitkeep, which the list hides.
        store.new_folder(&c, me, "plans").unwrap();
        let list = store.list_files(&c).unwrap();
        assert_eq!(list.folders, ["forms", "plans"]);
        assert_eq!(list.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["forms/consent.txt"]);
        assert!(repo.join("plans/.gitkeep").is_file());
        assert!(store.new_folder(&c, me, "plans").unwrap_err().contains("is a folder"));
        assert!(store.new_folder(&c, me, "forms/consent.txt/x").unwrap_err().contains("is a file, so nothing can go inside it"));

        // Rename, then move a folder: everything in it goes, the old folder is gone from disk.
        store.move_file(&c, me, "forms/consent.txt", "forms/consent-form.txt").unwrap();
        store.write_file(&c, me, "forms/old/2025.txt", b"old".to_vec()).unwrap();
        store.move_file(&c, "ava", "forms", "plans/forms").unwrap();
        assert_eq!(paths(&store, &c), ["plans/forms/consent-form.txt", "plans/forms/old/2025.txt"]);
        assert!(!repo.join("forms").exists(), "an emptied folder leaves the disk");
        assert_eq!(std::fs::read_to_string(repo.join("plans/forms/consent-form.txt")).unwrap(), "Sign here, twice.");
        let moved = store.file_history(&c, "plans/forms/consent-form.txt").unwrap();
        assert_eq!((moved[0].author.as_str(), moved[0].message.as_str()), ("ava", "Move forms to plans/forms"));
        // A folder's history is every change inside it: made, then the move in.
        assert_eq!(store.file_history(&c, "plans").unwrap().iter().map(|h| h.message.as_str()).collect::<Vec<_>>(), ["Move forms to plans/forms", "New folder plans"]);
        git_clean(&store, &c);

        // Refused moves: onto something, into itself, from nowhere.
        assert!(store.move_file(&c, me, "plans/forms/old/2025.txt", "plans/forms/consent-form.txt").unwrap_err().contains("already a file"));
        assert!(store.move_file(&c, me, "plans", "plans/inside").unwrap_err().contains("inside itself"));
        assert!(store.move_file(&c, me, "nope.txt", "x.txt").unwrap_err().contains("nothing at nope.txt"));
        assert!(store.move_file(&c, me, "plans/forms/old/2025.txt", "plans/forms/old/2025.txt").unwrap_err().contains("already there"));

        // Delete a file, then a folder: gone from the list and the disk, kept in the history.
        store.delete_file(&c, me, "plans/forms/old/2025.txt").unwrap();
        assert!(!repo.join("plans/forms/old").exists());
        store.delete_file(&c, "ava", "plans").unwrap();
        assert_eq!(paths(&store, &c), Vec::<String>::new());
        assert!(!repo.join("plans").exists());
        let gone = store.file_history(&c, "plans/forms/consent-form.txt").unwrap();
        assert_eq!((gone[0].author.as_str(), gone[0].message.as_str()), ("ava", "Delete plans"));
        assert!(store.delete_file(&c, me, "plans").unwrap_err().contains("nothing at plans"));
        git_clean(&store, &c);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hostile_paths_are_refused_before_anything_is_written() {
        let (dir, store, c) = store();
        let me = DEMO_NAME;
        let repo = store.repo(&c).unwrap();
        let before = records::head_commit(&repo).unwrap();
        for bad in ["../outside.txt", "/etc/passwd", "a/../../b", "./x", "a//b", "", ".git/config", "x/.GIT/hooks/pre-commit", "a\\b", "bad\u{0}name", "docs/\n.md"] {
            let e = store.write_file(&c, me, bad, b"x".to_vec()).unwrap_err();
            assert!(e.contains("isn't a path gild writes"), "{bad}: {e}");
            assert!(store.new_folder(&c, me, bad).is_err() && store.delete_file(&c, me, bad).is_err() && store.move_file(&c, me, "x", bad).is_err(), "{bad}");
        }
        // gild's own files: the access list above all.
        for own in [".gild/acl.json", ".gild/records/x.json", "gild.json", ".gitignore", ".gild"] {
            let e = store.write_file(&c, me, own, b"{}".to_vec()).unwrap_err();
            assert!(e.contains("gild's own files"), "{own}: {e}");
            assert!(store.delete_file(&c, me, own).unwrap_err().contains("gild's own files"));
            assert!(store.move_file(&c, me, own, "x").unwrap_err().contains("gild's own files"));
        }
        assert_eq!(records::head_commit(&repo).unwrap(), before, "nothing was committed");
        assert!(!dir.join("outside.txt").exists());

        // A link in the folder: writes through it are refused, the link isn't followed.
        #[cfg(unix)]
        {
            let outside = dir.join("elsewhere");
            std::fs::create_dir_all(&outside).unwrap();
            std::os::unix::fs::symlink(&outside, repo.join("linked")).unwrap();
            let e = store.write_file(&c, me, "linked/escape.txt", b"x".to_vec()).unwrap_err();
            assert!(e.contains("goes through a link"), "{e}");
            assert!(!outside.join("escape.txt").exists());
            std::fs::remove_file(repo.join("linked")).unwrap();
        }

        // Names this computer wouldn't tell apart: refused, not written over.
        store.write_file(&c, me, "Readme.md", b"# One\n".to_vec()).unwrap();
        let e = store.write_file(&c, me, "readme.md", b"# Two\n".to_vec()).unwrap_err();
        assert!(e.contains("Readme.md is already there"), "{e}");
        assert!(store.new_folder(&c, me, "README.MD").unwrap_err().contains("already there"));
        store.new_folder(&c, me, "Plans").unwrap();
        assert!(store.write_file(&c, me, "plans/x.md", b"x".to_vec()).unwrap_err().contains("Plans is already there"));
        assert!(store.move_file(&c, me, "Readme.md", "readme.md").unwrap_err().contains("Move it to another name first"));
        assert_eq!(std::fs::read_to_string(repo.join("Readme.md")).unwrap(), "# One\n");
        // Too big.
        let e = store.write_file(&c, me, "big.bin", vec![0; MAX_OUTSIDE_FILE as usize + 1]).unwrap_err();
        assert!(e.contains("at most 50 MB"), "{e}");
        git_clean(&store, &c);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn uploads_previews_and_the_grant() {
        let (dir, store, c) = store();
        let me = DEMO_NAME;
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3];
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        let up = |name: &str, bytes: &[u8]| Upload { name: name.into(), data: b64(bytes) };
        let got = store.upload_files(&c, me, "pictures", &[up("logo.png", &png), up("notes.txt", b"hello")]).unwrap();
        assert_eq!(got.iter().map(|f| (f.path.as_str(), f.size)).collect::<Vec<_>>(), [("pictures/logo.png", 11), ("pictures/notes.txt", 5)]);
        let log = store.file_history(&c, "pictures").unwrap();
        assert_eq!(log[0].message, "Add pictures/logo.png, pictures/notes.txt");
        let image = store.file_preview(&c, "pictures/logo.png").unwrap();
        assert_eq!((image.kind, image.size, image.text.is_none()), ("image", 11, true));
        assert_eq!(image.data.as_deref(), Some(format!("data:image/png;base64,{}", b64(&png)).as_str()));
        let text = store.file_preview(&c, "pictures/notes.txt").unwrap();
        assert_eq!((text.kind, text.text.as_deref(), text.data.is_none()), ("text", Some("hello"), true));
        store.write_file(&c, me, "blob.bin", vec![0xff, 0xfe, 0]).unwrap();
        let other = store.file_preview(&c, "blob.bin").unwrap();
        assert_eq!((other.kind, other.text.is_none(), other.data.is_none()), ("other", true, true));
        assert!(store.file_preview(&c, "../blob.bin").unwrap_err().contains("isn't a path gild writes"));
        assert!(store.file_preview(&c, "missing.txt").unwrap_err().contains("no file missing.txt"));
        assert!(store.file_location(&c, "blob.bin").unwrap().ends_with("blob.bin"));
        assert!(store.file_location(&c, "missing.txt").is_err());
        // Uploads refused whole: a bad name, a second copy, something already there, bytes that aren't base64.
        assert!(store.upload_files(&c, me, "", &[up("a/b.txt", b"x")]).unwrap_err().contains("isn't a file name"));
        assert!(store.upload_files(&c, me, "", &[up("x.txt", b"x"), up("X.TXT", b"y")]).unwrap_err().contains("twice"));
        assert!(store.upload_files(&c, me, "pictures", &[up("logo.png", b"x")]).unwrap_err().contains("already a file"));
        assert!(store.upload_files(&c, me, "", &[Upload { name: "x.txt".into(), data: "not base64!".into() }]).unwrap_err().contains("didn't arrive whole"));
        assert!(store.upload_files(&c, me, ".gild", &[up("acl.json", b"{}")]).unwrap_err().contains("gild's own files"));
        assert_eq!(paths(&store, &c), ["blob.bin", "pictures/logo.png", "pictures/notes.txt"]);

        // Without its grant, Files changes nothing, for anyone; reading still works.
        store.set_addon_grant(FILES, "files", "*", false).unwrap();
        let refused = "The Files add-on isn't allowed to change files (Settings › Add-ons › Files › Permissions).";
        assert_eq!(store.write_file(&c, me, "new.txt", b"x".to_vec()).unwrap_err(), refused);
        assert_eq!(store.delete_file(&c, me, "blob.bin").unwrap_err(), refused);
        assert_eq!(store.move_file(&c, me, "blob.bin", "b.bin").unwrap_err(), refused);
        assert_eq!(store.new_folder(&c, me, "f").unwrap_err(), refused);
        assert_eq!(store.upload_files(&c, me, "", &[up("y.txt", b"y")]).unwrap_err(), refused);
        assert_eq!(store.file_preview(&c, "pictures/notes.txt").unwrap().text.as_deref(), Some("hello"));
        assert_eq!(paths(&store, &c), ["blob.bin", "pictures/logo.png", "pictures/notes.txt"]);
        store.set_addon_grant(FILES, "files", "*", true).unwrap();
        store.delete_file(&c, me, "blob.bin").unwrap();

        // The shared view follows: a deleted open file closes, a .md written here is a doc.
        store.drive(&c, &Actor::Person(me.into()), &Nav::Open("pictures/notes.txt".into())).unwrap();
        store.delete_file(&c, me, "pictures/notes.txt").unwrap();
        assert_eq!(store.view(&c).unwrap().state.open, None);
        store.write_file(&c, me, "docs/plan.md", b"# Plan\n\nStep one.\n".to_vec()).unwrap();
        assert_eq!(store.doc(&c, "docs/plan.md").unwrap().title, "Plan");
        git_clean(&store, &c);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn panes_are_per_channel_in_its_history() {
        let (dir, store, c) = store();
        let me = DEMO_NAME;
        let shown = |panes: &[Pane]| panes.iter().map(|p| format!("{}{}", p.name, if p.hidden { "-" } else { "+" })).collect::<Vec<_>>();
        assert_eq!(shown(&store.channel_panes(&c).unwrap()), ["Docs+", "Files+"], "every add-on with a pane, shown, to start");
        let set = |panes: &[(&str, bool)]| panes.iter().map(|(a, h)| PaneSetting { addon: a.to_string(), hidden: *h }).collect::<Vec<_>>();
        // Hide Files; put it first; an add-on left out stays, shown.
        let now = store.set_channel_panes(&c, me, &set(&[("gild.files", true)])).unwrap();
        assert_eq!(shown(&now), ["Files-", "Docs+"]);
        let log = store.file_history(&c, ADDONS_PATH).unwrap();
        assert_eq!((log[0].author.as_str(), log[0].message.as_str()), ("Demo", "Panes: Files (hidden), Docs"));
        assert_eq!(store.set_channel_panes(&c, me, &set(&[("gild.files", true)])).unwrap(), now, "the same again is no commit");
        assert_eq!(store.file_history(&c, ADDONS_PATH).unwrap().len(), 1);
        assert!(store.set_channel_panes(&c, me, &set(&[("gild.nope", false)])).unwrap_err().contains("no add-on pane called gild.nope"));
        assert!(store.set_channel_panes(&c, me, &set(&[("gild.files", false), ("gild.files", true)])).unwrap_err().contains("twice"));
        // It's the channel's: another channel is untouched, and the file is one of gild's own, not Files'.
        let ws = store.workspaces().unwrap()[0].clone();
        let other = store.create_channel(&ws.id, "other", "").unwrap();
        assert_eq!(shown(&store.channel_panes(&other.id).unwrap()), ["Docs+", "Files+"]);
        assert!(!paths(&store, &c).iter().any(|p| p.starts_with(".gild")));
        assert!(store.delete_file(&c, me, ADDONS_PATH).unwrap_err().contains("gild's own files"));
        // Reopening reads it back from the repository.
        let data = dir.clone();
        drop(store);
        let store = Store::open(&data).unwrap();
        assert_eq!(shown(&store.channel_panes(&c).unwrap()), ["Files-", "Docs+"]);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
