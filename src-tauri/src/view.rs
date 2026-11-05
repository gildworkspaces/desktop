//! A channel's view as one shared computer: one state (which folders are
//! open, what's selected, which file is open) that the window draws and
//! agents drive, and one controller at a time.
//!
//! People and agents change it through the same operations. A person always
//! wins: anything a person does takes the view. An agent drives only after
//! `take`, and keeps it while it acts; a lease left idle for LEASE_MS ends,
//! as does a person's turn, so nobody holds the view by walking away.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::records::FileEntry;

pub const LEASE_MS: i64 = 60_000;

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Controller {
    #[default]
    None,
    Person { name: String, at: i64 },
    Agent { name: String, at: i64 },
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
pub struct ViewState {
    pub expanded: BTreeSet<String>,
    pub selected: Option<String>,
    pub open: Option<String>,
    pub controller: Controller,
}

/// Who is acting.
#[derive(Clone, Debug)]
pub enum Actor {
    Person(String),
    Agent(String),
}

/// What can be done to the view.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "op", content = "path", rename_all = "snake_case")]
pub enum Nav {
    Expand(String),
    Collapse(String),
    Select(String),
    Open(String),
    Close,
}

fn is_folder(files: &[FileEntry], path: &str) -> bool {
    let prefix = format!("{}/", path.trim_end_matches('/'));
    files.iter().any(|f| f.path.starts_with(&prefix))
}

fn is_file(files: &[FileEntry], path: &str) -> bool {
    files.iter().any(|f| f.path == path)
}

/// Every folder above a path: "a/b/c.md" → ["a", "a/b"].
fn parents(path: &str) -> Vec<String> {
    let parts: Vec<&str> = path.split('/').collect();
    (1..parts.len()).map(|n| parts[..n].join("/")).collect()
}

impl ViewState {
    fn who(&self) -> Option<&str> {
        match &self.controller {
            Controller::None => None,
            Controller::Person { name, .. } | Controller::Agent { name, .. } => Some(name),
        }
    }

    fn idle(&self, now: i64) -> bool {
        match &self.controller {
            Controller::None => true,
            Controller::Person { at, .. } | Controller::Agent { at, .. } => now - at > LEASE_MS,
        }
    }

    /// An agent asks for the view.
    pub fn take(&mut self, agent: &str, now: i64) -> Result<(), String> {
        let mine = matches!(&self.controller, Controller::Agent { name, .. } if name == agent);
        if !mine && !self.idle(now) {
            let who = self.who().unwrap_or("someone");
            let whom = if matches!(self.controller, Controller::Person { .. }) { who.to_string() } else { format!("@{who}") };
            return Err(format!("{whom} is using the view. Try again in a minute, or ask in the channel."));
        }
        self.controller = Controller::Agent { name: agent.into(), at: now };
        Ok(())
    }

    /// An agent gives the view back.
    pub fn release(&mut self, agent: &str) {
        if matches!(&self.controller, Controller::Agent { name, .. } if name == agent) {
            self.controller = Controller::None;
        }
    }

    /// May this actor act on the view now? A person always may (and takes
    /// it); an agent must hold it.
    pub fn claim(&mut self, actor: &Actor, now: i64) -> Result<(), String> {
        match actor {
            Actor::Person(name) => {
                self.controller = Controller::Person { name: name.clone(), at: now };
                Ok(())
            }
            Actor::Agent(agent) => match &self.controller {
                Controller::Agent { name, at } if name == agent && now - at <= LEASE_MS => {
                    self.controller = Controller::Agent { name: agent.clone(), at: now };
                    Ok(())
                }
                Controller::Person { name, .. } => Err(format!("{name} took over the view. Ask in the channel, or take_view again later.")),
                _ => Err("Take the view first with take_view.".into()),
            },
        }
    }

    /// Apply a navigation, checked against the files that exist.
    pub fn apply(&mut self, files: &[FileEntry], nav: &Nav) -> Result<(), String> {
        match nav {
            Nav::Expand(p) | Nav::Collapse(p) if !is_folder(files, p) => Err(format!("There's no folder {p} in this channel.")),
            Nav::Expand(p) => {
                self.expanded.extend(parents(p));
                self.expanded.insert(p.trim_end_matches('/').to_string());
                Ok(())
            }
            Nav::Collapse(p) => {
                let p = p.trim_end_matches('/');
                self.expanded.retain(|e| e != p && !e.starts_with(&format!("{p}/")));
                Ok(())
            }
            Nav::Select(p) if !is_file(files, p) && !is_folder(files, p) => Err(format!("There's nothing at {p} in this channel.")),
            Nav::Select(p) => {
                self.expanded.extend(parents(p));
                self.selected = Some(p.clone());
                Ok(())
            }
            Nav::Open(p) if !is_file(files, p) => Err(format!("There's no file {p} in this channel.")),
            Nav::Open(p) => {
                self.expanded.extend(parents(p));
                self.selected = Some(p.clone());
                self.open = Some(p.clone());
                Ok(())
            }
            Nav::Close => {
                self.open = None;
                Ok(())
            }
        }
    }

    /// Forget what no longer exists (a file deleted, a folder emptied).
    pub fn prune(&mut self, files: &[FileEntry]) {
        self.expanded.retain(|e| is_folder(files, e));
        if self.selected.as_deref().is_some_and(|s| !is_file(files, s) && !is_folder(files, s)) {
            self.selected = None;
        }
        if self.open.as_deref().is_some_and(|o| !is_file(files, o)) {
            self.open = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<FileEntry> {
        ["docs/intake-sop.md", "docs/guides/forms.md", "notes.md"].iter().map(|p| FileEntry { path: p.to_string(), size: 1 }).collect()
    }

    #[test]
    fn one_controller_and_a_person_always_wins() {
        let mut v = ViewState::default();
        let ava = Actor::Agent("ava".into());
        assert_eq!(v.claim(&ava, 0).unwrap_err(), "Take the view first with take_view.");
        v.take("ava", 0).unwrap();
        assert!(v.claim(&ava, 1_000).is_ok());
        // bob can't take it while ava is using it…
        assert_eq!(v.take("bob", 2_000).unwrap_err(), "@ava is using the view. Try again in a minute, or ask in the channel.");
        // …but a person can, at once, and ava's next step is refused.
        v.claim(&Actor::Person("Sami".into()), 3_000).unwrap();
        assert_eq!(v.claim(&ava, 4_000).unwrap_err(), "Sami took over the view. Ask in the channel, or take_view again later.");
        assert!(v.take("ava", 5_000).unwrap_err().starts_with("Sami is using the view"));
        // A person who walks away gives it up after the lease.
        v.take("ava", 3_000 + LEASE_MS + 1).unwrap();
        // So does an idle agent.
        v.take("bob", 3_000 + 2 * LEASE_MS + 2).unwrap();
        assert!(v.claim(&ava, 3_000 + 2 * LEASE_MS + 3).is_err());
        v.release("ava");
        assert!(matches!(&v.controller, Controller::Agent { name, .. } if name == "bob"), "only the holder releases");
        v.release("bob");
        assert_eq!(v.controller, Controller::None);
    }

    #[test]
    fn navigation_is_checked_against_the_files() {
        let mut v = ViewState::default();
        let f = files();
        v.apply(&f, &Nav::Open("docs/guides/forms.md".into())).unwrap();
        assert_eq!(v.expanded.iter().cloned().collect::<Vec<_>>(), ["docs", "docs/guides"], "opening reveals its folders");
        assert_eq!((v.selected.as_deref(), v.open.as_deref()), (Some("docs/guides/forms.md"), Some("docs/guides/forms.md")));
        v.apply(&f, &Nav::Collapse("docs".into())).unwrap();
        assert!(v.expanded.is_empty(), "closing a folder closes the ones in it");
        assert!(v.apply(&f, &Nav::Open("docs".into())).unwrap_err().contains("no file docs"));
        assert!(v.apply(&f, &Nav::Expand("notes.md".into())).unwrap_err().contains("no folder notes.md"));
        assert!(v.apply(&f, &Nav::Select("../etc/passwd".into())).is_err());
        v.apply(&f, &Nav::Close).unwrap();
        assert_eq!(v.open, None);
        v.apply(&f, &Nav::Open("notes.md".into())).unwrap();
        v.prune(&f[..2]);
        assert_eq!((v.open.as_deref(), v.selected.as_deref()), (None, None), "a file that's gone isn't open");
    }
}
