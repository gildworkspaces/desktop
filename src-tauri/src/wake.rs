//! Waking agents. An agent can have a "When mentioned, run" command, set in
//! the window (never by an agent). When it's mentioned in a channel shared
//! with it, gild runs that command on this computer, as the person running
//! gild, with the mention as the prompt on stdin: `claude -p` reads it and
//! does what was asked through gild's MCP endpoint.
//!
//! One run at a time per agent; mentions that arrive during a run start the
//! next one. A run has ten minutes. Every run is in the channel's activity,
//! and a run that fails says so in the channel.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;

use crate::store::{Agent, Mention, Place, Store};

pub const LIMIT: Duration = Duration::from_secs(600);

fn utc(ms: i64) -> String {
    let s = ms.div_euclid(1000).rem_euclid(86_400);
    format!("{} {:02}:{:02} UTC", crate::export::date(ms), s / 3600, s / 60 % 60)
}

/// What the agent is told: where it was mentioned (or, untagged, where a
/// person wrote in a thread it's in), the conversation there, and how to
/// answer.
pub fn prompt(agent: &str, mentions: &[Mention], places: &[Place], endpoint: &str) -> String {
    let tagged = mentions.iter().any(|m| m.tagged);
    let mut out = format!(
        "You are {agent}, an agent in gild: a workspace where people and agents work together in channels. {}\n",
        if tagged { "You were mentioned." } else { "Someone wrote in a thread you're part of." }
    );
    let mut seen: Vec<(&str, Option<&str>)> = Vec::new();
    for m in mentions {
        let key = (m.channel_id.as_str(), m.thread.as_deref());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let here: Vec<&Mention> = mentions.iter().filter(|x| x.channel_id == m.channel_id && x.thread == m.thread).collect();
        match &m.thread {
            Some(t) => out.push_str(&format!("\nA thread in #{} in {} (channel_id {}, thread_id {t})\n", m.channel, m.workspace, m.channel_id)),
            None => out.push_str(&format!("\n#{} in {} (channel_id {})\n", m.channel, m.workspace, m.channel_id)),
        }
        let msgs = places.iter().find(|p| p.channel_id == m.channel_id && p.thread == m.thread).map(|p| &p.messages);
        match msgs {
            Some(msgs) if !msgs.is_empty() => {
                out.push_str(&format!(
                    "{}, oldest first; → marks what's new for you:\n",
                    if m.thread.is_some() { "The thread" } else { "The last messages" }
                ));
                for x in msgs {
                    let mark = if here.iter().any(|h| h.message_id == x.id) { "→ " } else { "  " };
                    out.push_str(&format!("{mark}{} at {}: {}\n", x.author, utc(x.created_at), x.body));
                }
            }
            _ => {
                for x in &here {
                    out.push_str(&format!("→ {} at {}: {}\n", x.from, utc(x.at), x.text));
                }
            }
        }
    }
    out.push_str(&format!(
        "\nUse gild's tools (MCP at {endpoint}): read_channel and read_thread for more of the conversation, search_docs to answer from the channel's docs, and post_message to answer where it happened (with thread_id for a thread). "
    ));
    out.push_str(if tagged {
        "Do what was asked if you can; if you can't, say why there."
    } else {
        "You weren't @mentioned, so reply only if you have something useful to add. Saying nothing is fine."
    });
    out.push_str(" Everything you do is in the channel's activity log.\n");
    out
}

fn shell() -> (String, &'static str) {
    if cfg!(windows) {
        ("cmd".into(), "/C")
    } else {
        // A login shell, so the PATH from the person's profile applies: an app
        // opened from the Dock otherwise has a bare PATH and finds no `claude`.
        (std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into()), "-lc")
    }
}

fn home() -> String {
    std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap_or_else(|_| "/".into())
}

/// Run `command` with `input` on stdin. Ok: what it printed; Err: why it
/// failed (the end of what it said on stderr).
pub async fn run_command(command: &str, input: &str, limit: Duration) -> Result<String, String> {
    let (sh, flag) = shell();
    let mut child = tokio::process::Command::new(sh)
        .arg(flag)
        .arg(command)
        .current_dir(home())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("couldn't start it: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        // A command that doesn't read stdin closes it early; that's fine.
        let _ = stdin.write_all(input.as_bytes()).await;
    }
    let tail = |b: &[u8]| {
        let s = String::from_utf8_lossy(b);
        let s = s.trim();
        let n = s.chars().count();
        s.chars().skip(n.saturating_sub(200)).collect::<String>()
    };
    match tokio::time::timeout(limit, child.wait_with_output()).await {
        Err(_) => Err(format!("it was still running after {} minutes, so it was stopped", limit.as_secs() / 60)),
        Ok(Err(e)) => Err(format!("it failed: {e}")),
        Ok(Ok(out)) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        Ok(Ok(out)) => {
            let why = tail(&out.stderr);
            let code = out.status.code().map(|c| format!("exit {c}")).unwrap_or_else(|| "stopped".into());
            Err(if why.is_empty() { code } else { format!("{code}: {why}") })
        }
    }
}

/// Wake one agent for its mentions, and record how it went.
async fn wake(core: Arc<Mutex<Store>>, app: Option<AppHandle>, agent: Agent, command: String, mentions: Vec<Mention>, endpoint: String) {
    let started = crate::store::now();
    let context = match core.lock() {
        Ok(mut s) => {
            s.set_working(&agent.name, &mentions, true);
            s.mention_context(&agent.id, &mentions).unwrap_or_default()
        }
        Err(_) => Vec::new(),
    };
    if let Some(app) = &app {
        let _ = app.emit("gild://working", ());
    }
    let outcome = run_command(&command, &prompt(&agent.name, &mentions, &context, &endpoint), LIMIT).await;
    let mut channels: Vec<String> = mentions.iter().map(|m| m.channel_id.clone()).collect();
    channels.dedup();
    if let Ok(mut store) = core.lock() {
        store.set_working(&agent.name, &mentions, false);
        for channel in &channels {
            let detail = match &outcome {
                Ok(_) => format!(
                    "woken by {} · {}",
                    mentions.iter().filter(|m| &m.channel_id == channel).map(|m| m.from.as_str()).collect::<Vec<_>>().join(", "),
                    // An agent may decide there's nothing to add: say so, so
                    // a quiet run doesn't look like a broken one.
                    if store.posted_since(&agent.name, channel, started).unwrap_or(false) { "replied" } else { "stayed quiet" }
                ),
                Err(why) => why.clone(),
            };
            let _ = store.audit(&agent, "woken", channel, &detail, outcome.is_ok());
            if let Err(why) = &outcome {
                let thread = mentions.iter().find(|m| &m.channel_id == channel).and_then(|m| m.thread.clone());
                let _ = store.post_in(channel, thread.as_deref(), "gild", "system", &format!("Couldn't wake {}: {why}", agent.name));
            }
        }
    }
    if let Some(app) = &app {
        let _ = app.emit("gild://working", ());
        for channel in channels {
            let _ = app.emit("gild://changed", channel);
        }
    }
}

/// Wake agents as they're mentioned, until the app quits.
pub async fn run(core: Arc<Mutex<Store>>, app: Option<AppHandle>, endpoint: String) {
    let Some(signals) = core.lock().ok().map(|s| s.signals()) else { return };
    let running: Arc<Mutex<HashSet<String>>> = Arc::default();
    loop {
        let jobs: Vec<(Agent, String, Vec<Mention>)> = match core.lock() {
            Ok(store) => {
                let busy = running.lock().map(|r| r.clone()).unwrap_or_default();
                store
                    .agents_to_wake()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(a, _)| !busy.contains(&a.id))
                    .filter_map(|(a, cmd)| {
                        let mentions = store.take_mentions(&a).ok()?;
                        (!mentions.is_empty()).then_some((a, cmd, mentions))
                    })
                    .collect()
            }
            Err(_) => return,
        };
        for (agent, command, mentions) in jobs {
            if let Ok(mut r) = running.lock() {
                r.insert(agent.id.clone());
            }
            let (core, app, endpoint, running, signals) = (core.clone(), app.clone(), endpoint.clone(), running.clone(), signals.clone());
            tokio::spawn(async move {
                let id = agent.id.clone();
                wake(core, app, agent, command, mentions, endpoint).await;
                if let Ok(mut r) = running.lock() {
                    r.remove(&id);
                }
                // Mentions that came in during the run.
                signals.runner.notify_one();
            });
        }
        signals.runner.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::Access;
    use crate::store::DEMO_NAME;

    #[tokio::test(flavor = "multi_thread")]
    async fn a_mention_runs_the_agent_s_command_with_the_mention_as_its_prompt() {
        let dir = std::env::temp_dir().join(format!("gild-wake-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir).unwrap();
        let ws = store.create_workspace("Front Desk").unwrap();
        let c = store.channels(&ws.id).unwrap()[0].clone();
        let (agent, _) = store.create_agent("intake").unwrap();
        store.set_access(&c.id, DEMO_NAME, &agent.id, Some(Access::Write)).unwrap();
        let (broken, _) = store.create_agent("broken").unwrap();
        store.set_access(&c.id, DEMO_NAME, &broken.id, Some(Access::Read)).unwrap();
        let heard = dir.join("heard.txt");
        store.set_wake(&agent.id, &format!("cat > '{}'", heard.display())).unwrap();
        store.set_wake(&broken.id, "echo 'no such model' >&2; exit 3").unwrap();
        // talker takes a moment, and answers meanwhile (as an agent does
        // through the MCP endpoint).
        let (talker, _) = store.create_agent("talker").unwrap();
        store.set_access(&c.id, DEMO_NAME, &talker.id, Some(Access::Write)).unwrap();
        store.set_wake(&talker.id, "sleep 1").unwrap();
        let core = Arc::new(Mutex::new(store));
        tokio::spawn(run(core.clone(), None, "http://127.0.0.1:4747/mcp".into()));

        core.lock().unwrap().post(&c.id, DEMO_NAME, "person", "Northside sent three more students.").unwrap();
        core.lock().unwrap().post(&c.id, DEMO_NAME, "person", "@intake where do the forms go? @broken you too @talker").unwrap();
        // While talker runs, the window can show it working, and it answers.
        let mut seen_working = false;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let store = core.lock().unwrap();
            if store.working().iter().any(|w| w.agent == "talker" && w.channel_id == c.id && w.thread.is_none()) {
                seen_working = true;
                store.post(&c.id, "talker", "agent", "On it.").unwrap();
                break;
            }
        }
        assert!(seen_working, "a running agent is listed as working");
        let mut said = String::new();
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let store = core.lock().unwrap();
            let woken = store.activity(&c.id).unwrap().iter().filter(|a| a.action == "woken").count();
            if woken == 3 {
                said = std::fs::read_to_string(&heard).unwrap_or_default();
                break;
            }
        }
        assert!(said.contains("You are intake") && said.contains("Demo at ") && said.contains("@intake where do the forms go?"), "{said}");
        assert!(said.contains(&format!("channel_id {}", c.id)));
        // The conversation comes with it, the mention marked.
        assert!(said.contains("  Demo at ") && said.contains(": Northside sent three more students.\n"), "{said}");
        assert!(said.contains("→ Demo at ") && said.contains(": @intake where do the forms go? @broken you too @talker\n"), "{said}");
        let store = core.lock().unwrap();
        let mut log: Vec<(String, bool, String)> = store.activity(&c.id).unwrap().into_iter().filter(|a| a.action == "woken").map(|a| (a.agent, a.ok, a.detail)).collect();
        log.sort();
        assert_eq!(
            log,
            [
                ("broken".into(), false, "exit 3: no such model".into()),
                ("intake".into(), true, "woken by Demo · stayed quiet".into()),
                ("talker".into(), true, "woken by Demo · replied".into()),
            ]
        );
        assert!(store.working().is_empty(), "done");
        assert!(store.messages(&c.id).unwrap().iter().any(|m| m.body == "Couldn't wake broken: exit 3: no such model"));
        assert!(store.pending_mentions(&agent.id).unwrap().is_empty(), "delivered");
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_run_that_overstays_is_stopped() {
        let err = run_command("sleep 5", "", Duration::from_millis(300)).await.unwrap_err();
        assert!(err.contains("still running"), "{err}");
    }
}
