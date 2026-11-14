//! Notices files changed in workspace folders by other apps (an editor, a
//! file manager, `git` itself) while gild runs, and takes them in: one
//! commit as the person, then the index (`Store::check_outside`). A save
//! often touches a file several times, so changes are taken in once things
//! have been quiet for a moment. What changed while gild was closed is
//! found by the scan when the store opens.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::{EventKind, RecursiveMode, Watcher};

use crate::store::Store;

/// How long things stay quiet before gild looks.
const QUIET: Duration = Duration::from_millis(400);
/// The longest a run of changes waits, even if they keep coming.
const AT_MOST: Duration = Duration::from_secs(3);

/// Told the id of each channel whose files or problems changed.
pub type OnChange = Box<dyn Fn(&str) + Send + 'static>;

pub struct Watch {
    watcher: Mutex<notify::RecommendedWatcher>,
    watched: Mutex<HashSet<PathBuf>>,
}

impl Watch {
    /// Watch every workspace folder the store has, and take in what changes.
    pub fn start(core: Arc<Mutex<Store>>, on_change: OnChange) -> Result<Arc<Watch>, String> {
        let (tx, rx) = mpsc::channel::<PathBuf>();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            for path in event.paths {
                // git's own bookkeeping (gild's commits write there too).
                if !path.components().any(|c| c.as_os_str() == ".git") {
                    let _ = tx.send(path);
                }
            }
        })
        .map_err(|e| format!("watching workspace folders: {e}"))?;
        let watch = Arc::new(Watch { watcher: Mutex::new(watcher), watched: Mutex::new(HashSet::new()) });
        {
            let store = core.lock().map_err(|_| "the workspace database is unavailable; restart gild".to_string())?;
            watch.watch_all(&store);
        }
        std::thread::spawn(move || {
            while let Ok(first) = rx.recv() {
                let mut paths = vec![first];
                let started = Instant::now();
                while started.elapsed() < AT_MOST {
                    match rx.recv_timeout(QUIET) {
                        Ok(p) => paths.push(p),
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
                let Ok(store) = core.lock() else { return };
                let channels: BTreeSet<String> = paths.iter().filter_map(|p| store.channel_at(p).ok().flatten()).collect();
                for c in channels {
                    let changed = match store.check_outside(&c) {
                        Ok(changed) => changed,
                        Err(e) => store.set_problems(&c, vec![e]),
                    };
                    if changed {
                        on_change(&c);
                    }
                }
            }
        });
        Ok(watch)
    }

    /// Start watching any workspace folder that isn't watched yet (a new
    /// or imported workspace).
    pub fn watch_all(&self, store: &Store) {
        let (Ok(mut watcher), Ok(mut watched)) = (self.watcher.lock(), self.watched.lock()) else { return };
        for (_, root) in store.workspace_folders().unwrap_or_default() {
            if !watched.contains(&root) && root.is_dir() && watcher.watch(&root, RecursiveMode::Recursive).is_ok() {
                watched.insert(root);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::DEMO_NAME;

    #[test]
    fn a_file_saved_by_another_app_is_taken_in_while_gild_runs() {
        let base = std::env::temp_dir().join(format!("gild-watch-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("data")).unwrap();
        let store = Store::open(&base.join("data")).unwrap();
        let ws = store.create_workspace("W").unwrap();
        let intake = store.create_channel(&ws.id, "intake", "").unwrap();
        let dir = store.workspace_folder(&ws.id).unwrap().join("channels/intake");
        let core = Arc::new(Mutex::new(store));
        let (tx, rx) = mpsc::channel::<String>();
        let tx = Mutex::new(tx);
        let _watch = Watch::start(core.clone(), Box::new(move |c| { let _ = tx.lock().unwrap().send(c.to_string()); })).unwrap();
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("docs/from-elsewhere.md"), "# From elsewhere\n").unwrap();
        // Told once it's in; then the index has it, written as the person.
        let told = rx.recv_timeout(Duration::from_secs(20)).expect("the change was noticed");
        assert_eq!(told, intake.id);
        let store = core.lock().unwrap();
        assert_eq!(store.doc(&intake.id, "docs/from-elsewhere.md").unwrap().title, "From elsewhere");
        let log = store.doc_history(&intake.id, "docs/from-elsewhere.md").unwrap();
        assert_eq!((log[0].author.as_str(), log[0].message.as_str()), (DEMO_NAME, "Edit outside gild: docs/from-elsewhere.md"));
        drop(store);
        let _ = std::fs::remove_dir_all(base);
    }
}
