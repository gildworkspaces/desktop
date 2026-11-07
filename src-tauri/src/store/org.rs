//! Orgs: who works with whom, as a tree of seats.
//!
//! A seat has a title and reports to one other seat; the top seat leads the
//! org. A seat is held by a person, an agent, or nobody yet (an open seat),
//! so everything that points at a seat keeps working when it's filled: a
//! person who joins later takes a seat and brings their agents as the team
//! under it. A team is a seat and every seat under it. The tree is at most
//! `MAX_LEVELS` deep, counting the top.
//!
//! Every workspace belongs to an org. A channel can be open to its
//! workspace's whole org, to teams in it, and to named agents (its
//! `.gild/acl.json`, so each change is a commit in the channel's history).

use serde::{Deserialize, Serialize};
use turso_core::Value;

use super::{new_id, now_ms, parse_access, text, Store};
use crate::records::{self, Access, Acl};

/// The top seat and four levels under it.
pub const MAX_LEVELS: usize = 5;
const MAX_TITLE: usize = 60;

/// Who holds a seat.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "id", rename_all = "lowercase")]
pub enum Holder {
    Person(String),
    Agent(String),
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Seat {
    pub id: String,
    pub title: String,
    /// The seat it reports to; none for the top seat.
    pub parent: Option<String>,
    /// None: open, waiting for someone.
    pub holder: Option<Holder>,
    /// 1 for the top seat.
    pub level: usize,
    /// How many seats its team has, itself included.
    pub team: usize,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Org {
    pub id: String,
    pub name: String,
    /// The workspaces that belong to it.
    pub workspaces: Vec<String>,
    /// In outline order: each seat, then its reports (oldest first).
    pub seats: Vec<Seat>,
    /// How deep the tree may go (`MAX_LEVELS`), for the window.
    pub max_levels: usize,
}

/// Why an agent can be in a channel.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Via {
    /// Named in the channel's access.
    Named,
    /// It holds a seat in the org, and the channel is open to the org.
    Org,
    /// It's in the team under `seat`, which the channel is open to.
    Team { seat: String },
}

/// A seat and the seats under it, from an outline.
pub fn team<'a>(seats: &'a [Seat], seat_id: &str) -> Vec<&'a Seat> {
    let Some(start) = seats.iter().position(|s| s.id == seat_id) else { return Vec::new() };
    let level = seats[start].level;
    let mut out = vec![&seats[start]];
    out.extend(seats[start + 1..].iter().take_while(|s| s.level > level));
    out
}

fn title_ok(title: &str) -> Result<String, String> {
    let t = title.trim();
    if t.is_empty() || t.chars().count() > MAX_TITLE {
        return Err(format!("A seat's title has 1 to {MAX_TITLE} characters."));
    }
    Ok(t.to_string())
}

struct Row {
    id: String,
    parent: Option<String>,
    title: String,
    holder: Option<Holder>,
}

fn holder(kind: &Value, id: &Value) -> Option<Holder> {
    match (text(kind).as_str(), text(id)) {
        ("person", id) => Some(Holder::Person(id)),
        ("agent", id) => Some(Holder::Agent(id)),
        _ => None,
    }
}

fn holder_values(h: &Option<Holder>) -> (Value, Value) {
    match h {
        Some(Holder::Person(id)) => (Value::from_text("person".to_string()), Value::from_text(id.clone())),
        Some(Holder::Agent(id)) => (Value::from_text("agent".to_string()), Value::from_text(id.clone())),
        None => (Value::Null, Value::Null),
    }
}

impl Store {
    /// You, as a holder of seats. Made once, kept.
    pub fn me_id(&self) -> Result<String, String> {
        if let Some(id) = self.setting("person_id")? {
            return Ok(id);
        }
        let id = new_id();
        self.set_setting("person_id", Some(&id))?;
        Ok(id)
    }

    /// The first org, made with you leading it if there's none, and every
    /// workspace not in an org put in it.
    pub(crate) fn ensure_org(&self) -> Result<String, String> {
        let org = match self.query("SELECT id FROM orgs ORDER BY created_at, id LIMIT 1", &[])?.first() {
            Some(r) => text(&r[0]),
            None => {
                let (id, at) = (new_id(), now_ms());
                self.run(
                    "INSERT INTO orgs (id, name, created_at) VALUES (?1, ?2, ?3)",
                    &[Value::from_text(id.clone()), Value::from_text(format!("{}'s org", self.me()?)), Value::from_i64(at)],
                )?;
                self.insert_seat(&id, None, "CEO", &Some(Holder::Person(self.me_id()?)))?;
                id
            }
        };
        self.run(
            "INSERT INTO org_workspaces (workspace_id, org_id) SELECT id, ?1 FROM workspaces WHERE id NOT IN (SELECT workspace_id FROM org_workspaces)",
            &[Value::from_text(org.clone())],
        )?;
        Ok(org)
    }

    fn insert_seat(&self, org_id: &str, parent: Option<&str>, title: &str, holder: &Option<Holder>) -> Result<String, String> {
        let id = new_id();
        let (kind, who) = holder_values(holder);
        self.run(
            "INSERT INTO seats (id, org_id, parent_id, title, holder_kind, holder_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            &[
                Value::from_text(id.clone()),
                Value::from_text(org_id.to_string()),
                parent.map(|p| Value::from_text(p.to_string())).unwrap_or(Value::Null),
                Value::from_text(title.to_string()),
                kind,
                who,
                Value::from_i64(now_ms()),
            ],
        )?;
        Ok(id)
    }

    /// An org's seats, in outline order with their levels.
    pub fn seats(&self, org_id: &str) -> Result<Vec<Seat>, String> {
        let rows: Vec<Row> = self
            .query(
                "SELECT id, parent_id, title, holder_kind, holder_id FROM seats WHERE org_id = ?1 ORDER BY created_at, id",
                &[Value::from_text(org_id.to_string())],
            )?
            .iter()
            .map(|r| Row {
                id: text(&r[0]),
                parent: match &r[1] {
                    Value::Null => None,
                    v => Some(text(v)),
                },
                title: text(&r[2]),
                holder: holder(&r[3], &r[4]),
            })
            .collect();
        fn walk(rows: &[Row], parent: Option<&str>, level: usize, out: &mut Vec<Seat>) {
            for r in rows.iter().filter(|r| r.parent.as_deref() == parent) {
                out.push(Seat { id: r.id.clone(), title: r.title.clone(), parent: r.parent.clone(), holder: r.holder.clone(), level, team: 1 });
                walk(rows, Some(&r.id), level + 1, out);
            }
        }
        let mut out = Vec::new();
        walk(&rows, None, 1, &mut out);
        let sizes: Vec<usize> = out.iter().map(|s| team(&out, &s.id).len()).collect();
        for (s, n) in out.iter_mut().zip(sizes) {
            s.team = n;
        }
        Ok(out)
    }

    /// Every org, with its workspaces and seats.
    pub fn orgs(&self) -> Result<Vec<Org>, String> {
        self.ensure_org()?;
        self.query("SELECT id, name FROM orgs ORDER BY created_at, id", &[])?
            .iter()
            .map(|r| {
                let id = text(&r[0]);
                let workspaces = self
                    .query("SELECT workspace_id FROM org_workspaces WHERE org_id = ?1", &[Value::from_text(id.clone())])?
                    .iter()
                    .map(|w| text(&w[0]))
                    .collect();
                Ok(Org { seats: self.seats(&id)?, id, name: text(&r[1]), workspaces, max_levels: MAX_LEVELS })
            })
            .collect()
    }

    fn seat_org(&self, seat_id: &str) -> Result<String, String> {
        self.query("SELECT org_id FROM seats WHERE id = ?1", &[Value::from_text(seat_id.to_string())])?
            .first()
            .map(|r| text(&r[0]))
            .ok_or_else(|| "That seat no longer exists.".to_string())
    }

    /// The org a channel's workspace belongs to.
    pub(crate) fn channel_org(&self, channel_id: &str) -> Result<Option<String>, String> {
        Ok(self
            .query(
                "SELECT o.org_id FROM channels c JOIN org_workspaces o ON o.workspace_id = c.workspace_id WHERE c.id = ?1",
                &[Value::from_text(channel_id.to_string())],
            )?
            .first()
            .map(|r| text(&r[0])))
    }

    pub fn rename_org(&self, org_id: &str, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 60 {
            return Err("An org's name has 1 to 60 characters.".into());
        }
        if self.query("SELECT 1 FROM orgs WHERE id = ?1", &[Value::from_text(org_id.to_string())])?.is_empty() {
            return Err("That org no longer exists.".into());
        }
        self.run("UPDATE orgs SET name = ?1 WHERE id = ?2", &[Value::from_text(name.to_string()), Value::from_text(org_id.to_string())])
    }

    /// A new, open seat reporting to `parent_id`.
    pub fn add_seat(&self, parent_id: &str, title: &str) -> Result<String, String> {
        let title = title_ok(title)?;
        let org = self.seat_org(parent_id)?;
        let seats = self.seats(&org)?;
        let parent = seats.iter().find(|s| s.id == parent_id).ok_or("That seat no longer exists.")?;
        if parent.level >= MAX_LEVELS {
            return Err(format!("{} is on level {MAX_LEVELS}, the deepest an org goes, so it can't have reports.", parent.title));
        }
        self.insert_seat(&org, Some(parent_id), &title, &None)
    }

    /// A seat's title and who holds it (None: open). A person or agent holds
    /// one seat in an org at most.
    pub fn set_seat(&self, seat_id: &str, title: &str, holder: Option<Holder>) -> Result<(), String> {
        let title = title_ok(title)?;
        let org = self.seat_org(seat_id)?;
        let who = match &holder {
            None => None,
            Some(Holder::Agent(id)) => Some(self.agent(id)?.name),
            Some(Holder::Person(id)) if *id == self.me_id()? => Some(self.me()?),
            Some(Holder::Person(_)) => return Err("That person isn't in gild yet.".into()),
        };
        if let (Some(who), Some(h)) = (who, &holder) {
            if let Some(other) = self.seats(&org)?.iter().find(|s| s.id != seat_id && s.holder.as_ref() == Some(h)) {
                return Err(format!("{who} already holds {}. Open that seat first.", other.title));
            }
        }
        let (kind, id) = holder_values(&holder);
        self.run(
            "UPDATE seats SET title = ?1, holder_kind = ?2, holder_id = ?3 WHERE id = ?4",
            &[Value::from_text(title), kind, id, Value::from_text(seat_id.to_string())],
        )
    }

    /// Have a seat, and its whole team with it, report to another seat.
    pub fn move_seat(&self, seat_id: &str, parent_id: &str) -> Result<(), String> {
        let org = self.seat_org(seat_id)?;
        if self.seat_org(parent_id)? != org {
            return Err("Seats can only move within their org.".into());
        }
        let seats = self.seats(&org)?;
        let moving = team(&seats, seat_id);
        let seat = moving.first().ok_or("That seat no longer exists.")?;
        if seat.parent.is_none() {
            return Err("The top seat leads the org; it doesn't report to anyone.".into());
        }
        if moving.iter().any(|s| s.id == parent_id) {
            return Err(format!("{} can't report to a seat in its own team.", seat.title));
        }
        let parent = seats.iter().find(|s| s.id == parent_id).ok_or("That seat no longer exists.")?;
        let depth = moving.iter().map(|s| s.level - seat.level + 1).max().unwrap_or(1);
        if parent.level + depth > MAX_LEVELS {
            return Err(format!("Under {}, {}'s team would go past level {MAX_LEVELS}.", parent.title, seat.title));
        }
        self.run("UPDATE seats SET parent_id = ?1 WHERE id = ?2", &[Value::from_text(parent_id.to_string()), Value::from_text(seat_id.to_string())])
    }

    /// Remove a seat; its reports report to its own parent instead.
    pub fn remove_seat(&self, seat_id: &str) -> Result<(), String> {
        let org = self.seat_org(seat_id)?;
        let seats = self.seats(&org)?;
        let seat = seats.iter().find(|s| s.id == seat_id).ok_or("That seat no longer exists.")?;
        let Some(parent) = &seat.parent else { return Err("The top seat leads the org, so it stays.".into()) };
        let (p, id) = (Value::from_text(parent.clone()), Value::from_text(seat_id.to_string()));
        self.run("UPDATE seats SET parent_id = ?1 WHERE parent_id = ?2", &[p, id.clone()])?;
        self.run("DELETE FROM seats WHERE id = ?1", &[id])
    }

    /// An agent that's removed leaves its seat open.
    pub(crate) fn vacate(&self, agent_id: &str) -> Result<(), String> {
        self.run(
            "UPDATE seats SET holder_kind = NULL, holder_id = NULL WHERE holder_kind = 'agent' AND holder_id = ?1",
            &[Value::from_text(agent_id.to_string())],
        )
    }

    /// The channel's access as its history has it now.
    pub(crate) fn current_acl(&self, channel_id: &str) -> Result<Acl, String> {
        let id = Value::from_text(channel_id.to_string());
        let mut acl = Acl::default();
        for row in self.query("SELECT agent_id, access FROM grants WHERE channel_id = ?1", std::slice::from_ref(&id))? {
            if let Some(a) = parse_access(&text(&row[1])) {
                acl.agents.insert(text(&row[0]), a);
            }
        }
        for row in self.query("SELECT kind, value, access FROM channel_rules WHERE channel_id = ?1 ORDER BY rowid", &[id])? {
            match text(&row[0]).as_str() {
                "org" => acl.org = true,
                "team" => acl.teams.push(text(&row[1])),
                "level" => {
                    if let Some(a) = parse_access(&text(&row[2])) {
                        acl.levels.insert(text(&row[1]), a);
                    }
                }
                _ => {}
            }
        }
        Ok(acl)
    }

    /// Index a channel's org and team rules from its history.
    pub(crate) fn index_rules(&self, channel_id: &str, acl: &Acl) -> Result<(), String> {
        let id = Value::from_text(channel_id.to_string());
        self.run("DELETE FROM channel_rules WHERE channel_id = ?1", std::slice::from_ref(&id))?;
        let mut rows: Vec<(&str, String, &str)> = Vec::new();
        if acl.org {
            rows.push(("org", String::new(), ""));
        }
        rows.extend(acl.teams.iter().map(|t| ("team", t.clone(), "")));
        rows.extend(acl.levels.iter().map(|(agent, a)| ("level", agent.clone(), if *a == Access::Write { "write" } else { "read" })));
        for (kind, value, access) in rows {
            self.run(
                "INSERT INTO channel_rules (channel_id, kind, value, access) VALUES (?1, ?2, ?3, ?4)",
                &[id.clone(), Value::from_text(kind.to_string()), Value::from_text(value), Value::from_text(access.to_string())],
            )?;
        }
        Ok(())
    }

    pub(crate) fn save_acl(&self, channel_id: &str, author: &str, message: &str, acl: &Acl) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(acl).map_err(|e| format!("saving access: {e}"))?;
        self.write(channel_id, author, message, &[(records::ACL_PATH.to_string(), bytes)])
    }

    /// What an agent may do in a channel, and why: named there, or in
    /// through the org or a team. Named access wins.
    pub fn reach(&self, agent_id: &str, channel_id: &str) -> Result<Option<(Access, Via)>, String> {
        let acl = self.current_acl(channel_id)?;
        if let Some(a) = acl.agents.get(agent_id) {
            return Ok(Some((*a, Via::Named)));
        }
        if !acl.org && acl.teams.is_empty() {
            return Ok(None);
        }
        let Some(org) = self.channel_org(channel_id)? else { return Ok(None) };
        let seats = self.seats(&org)?;
        let me = Holder::Agent(agent_id.to_string());
        let Some(mine) = seats.iter().find(|s| s.holder.as_ref() == Some(&me)) else { return Ok(None) };
        let level = acl.levels.get(agent_id).copied().unwrap_or(Access::Write);
        if let Some(t) = acl.teams.iter().find(|t| team(&seats, t).iter().any(|s| s.id == mine.id)) {
            return Ok(Some((level, Via::Team { seat: t.clone() })));
        }
        Ok(acl.org.then_some((level, Via::Org)))
    }

    /// Open a channel to its org, to teams in it, or neither (named agents
    /// stay as they are). A commit in the channel's history.
    pub fn set_rules(&self, channel_id: &str, author: &str, org: bool, teams: &[String]) -> Result<(), String> {
        self.require_channel(channel_id)?;
        let org_id = self.channel_org(channel_id)?.ok_or("This channel's workspace isn't in an org.")?;
        let seats = self.seats(&org_id)?;
        let name = |id: &str| seats.iter().find(|s| s.id == id).map(|s| s.title.clone());
        let mut wanted: Vec<String> = Vec::new();
        for t in teams {
            name(t).ok_or("That team isn't in this channel's org.")?;
            if !wanted.contains(t) {
                wanted.push(t.clone());
            }
        }
        let mut acl = self.current_acl(channel_id)?;
        let org_name = self.orgs()?.into_iter().find(|o| o.id == org_id).map(|o| o.name).unwrap_or_default();
        let mut said: Vec<String> = Vec::new();
        if acl.org != org {
            said.push(if org { format!("Open to everyone in {org_name}") } else { format!("Close to the rest of {org_name}") });
        }
        for t in wanted.iter().filter(|t| !acl.teams.contains(t)) {
            said.push(format!("Open to {}'s team", name(t).unwrap_or_default()));
        }
        for t in acl.teams.iter().filter(|t| !wanted.contains(t)) {
            said.push(format!("Close to {}'s team", name(t).unwrap_or_else(|| "a removed seat".into())));
        }
        if said.is_empty() {
            return Ok(());
        }
        acl.org = org;
        acl.teams = wanted;
        self.save_acl(channel_id, author, &said.join("; "), &acl)
    }

    /// Read or change for an agent in a channel, whether it's named there
    /// or in through the org or a team.
    pub fn set_level(&self, channel_id: &str, author: &str, agent_id: &str, access: Access) -> Result<(), String> {
        let agent = self.agent(agent_id)?;
        let mut acl = self.current_acl(channel_id)?;
        match self.reach(agent_id, channel_id)? {
            None => return Err(format!("{} isn't in this channel.", agent.name)),
            Some((have, _)) if have == access => return Ok(()),
            Some((_, Via::Named)) => {
                acl.agents.insert(agent_id.to_string(), access);
            }
            Some(_) => {
                acl.levels.insert(agent_id.to_string(), access);
            }
        }
        let message = match access {
            Access::Write => format!("Let {} read and change this channel", agent.name),
            Access::Read => format!("Let {} read this channel", agent.name),
        };
        self.save_acl(channel_id, author, &message, &acl)
    }
}
