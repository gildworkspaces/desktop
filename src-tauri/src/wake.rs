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

use serde_json::{json, Value};

use crate::mcp::Gild;
use crate::providers;
use crate::store::{Agent, Mention, Place, Runner, Store, Wake};

pub const LIMIT: Duration = Duration::from_secs(600);

fn utc(ms: i64) -> String {
    let s = ms.div_euclid(1000).rem_euclid(86_400);
    format!("{} {:02}:{:02} UTC", crate::export::date(ms), s / 3600, s / 60 % 60)
}

/// What the agent is told: where it was mentioned (or, untagged, where a
/// person wrote in a thread it's in), the conversation there, and how to
/// answer.
pub fn prompt(agent: &str, role: &str, mentions: &[Mention], places: &[Place], endpoint: &str) -> String {
    let tagged = mentions.iter().any(|m| m.tagged);
    let mut out = format!(
        "You are {agent}, an agent in gild: a workspace where people and agents work together in channels. {}\n",
        if tagged { "You were mentioned." } else { "Someone wrote in a thread you're part of." }
    );
    // What the agent's profile says it is and how it works.
    if !role.is_empty() {
        out.push_str(&format!("\nYour instructions, from your profile in gild:\n{role}\n"));
    }
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
/// The most model rounds a cloud agent gets per wake.
pub const MAX_STEPS: usize = 12;

const SYSTEM: &str = "You are an agent in gild, a workspace where people and agents work together in channels. What you write in reply is posted, as you write it, where you were mentioned (in the thread, if it was a thread), so answer in your reply; use post_message only to post somewhere else. Use the other tools to look things up or to act; everything you do is recorded under your name. Be brief.";

/// How often a reply being written is shown again in the window.
const STREAM_EVERY: Duration = Duration::from_millis(120);

/// A cloud agent's run: gild calls the provider's model, streams what it
/// writes into a reply where it was mentioned (`place`: channel and thread),
/// and runs the tool calls it asks for, as the MCP endpoint would, through
/// the same `call_as`.
async fn run_provider(core: &Arc<Mutex<Store>>, app: &Option<AppHandle>, agent: &Agent, runner: &Runner, prompt: &str, place: (&str, Option<&str>)) -> Result<(), String> {
    let (creds, off) = {
        let store = core.lock().map_err(|_| "the workspace database is unavailable".to_string())?;
        (store.creds(&runner.provider)?, store.tools_off(&agent.id)?)
    };
    // The agent's tools, as the provider takes them (waiting on mentions is
    // for agents that keep running; a cloud agent is woken instead).
    let tools: Vec<Value> = crate::mcp::tools()
        .into_iter()
        .filter(|t| t.name != "wait_for_mentions" && !off.iter().any(|o| o == t.name.as_ref()))
        .map(|t| json!({ "type": "function", "function": { "name": t.name, "description": t.description, "parameters": t.input_schema.as_ref() } }))
        .collect();
    let gild = Gild { core: core.clone(), app: app.clone() };
    // Calls already made this run: a model asking for the same thing again
    // isn't run twice.
    let mut done: Vec<(String, String)> = Vec::new();
    let mut messages = vec![json!({ "role": "system", "content": SYSTEM }), json!({ "role": "user", "content": prompt })];
    let tell = |channel: &str| {
        if let Some(app) = app {
            let _ = app.emit("gild://changed", channel);
        }
    };
    for _ in 0..MAX_STEPS {
        // The reply grows in the channel as the model writes it.
        let (mut draft, mut written, mut shown) = (None::<String>, String::new(), std::time::Instant::now() - STREAM_EVERY);
        let turn = providers::chat_stream(&creds, &runner.model, &messages, &tools, |piece| {
            written.push_str(piece);
            let Ok(store) = core.lock() else { return };
            if draft.is_none() {
                draft = store.start_draft(place.0, place.1, &agent.name).ok().map(|m| m.id);
            }
            if shown.elapsed() >= STREAM_EVERY {
                if let Some(id) = &draft {
                    let _ = store.update_draft(id, &written);
                }
                shown = std::time::Instant::now();
                drop(store);
                tell(place.0);
            }
        })
        .await;
        if let Some(id) = draft {
            if let Ok(store) = core.lock() {
                let _ = store.finish_draft(&id, &written);
            }
            tell(place.0);
        }
        let turn = turn?;
        messages.push(turn.message.clone());
        if turn.tool_calls.is_empty() {
            return Ok(());
        }
        let mut fresh = false;
        let mut answered = false;
        for call in &turn.tool_calls {
            let args = call.arguments.as_object().cloned().unwrap_or_default();
            let key = (call.name.clone(), Value::Object(args.clone()).to_string());
            let result = if done.contains(&key) {
                "error: you already did exactly this; don't repeat it. If you've answered, stop.".to_string()
            } else {
                fresh = true;
                done.push(key);
                match gild.call_as(agent, &call.name, &args).await {
                    Ok(v) => {
                        // Posting where it was mentioned is its answer.
                        let here = args.get("channel_id").and_then(Value::as_str) == Some(place.0)
                            && args.get("thread_id").and_then(Value::as_str).filter(|t| !t.is_empty()) == place.1;
                        answered |= call.name == "post_message" && here;
                        v.to_string()
                    }
                    Err(e) => format!("error: {e}"),
                }
            };
            messages.push(json!({ "role": "tool", "tool_call_id": call.id, "content": result }));
        }
        // It answered where it was asked, or it's only repeating itself: done.
        if answered || !fresh {
            return Ok(());
        }
    }
    Err(format!("it was still working after {MAX_STEPS} rounds, so gild stopped it"))
}

async fn wake(core: Arc<Mutex<Store>>, app: Option<AppHandle>, agent: Agent, how: Wake, mentions: Vec<Mention>, endpoint: String) {
    let started = crate::store::now();
    let (context, role) = match core.lock() {
        Ok(mut s) => {
            s.set_working(&agent.name, &mentions, true);
            (s.mention_context(&agent.id, &mentions).unwrap_or_default(), s.agent_prompt(&agent.id).unwrap_or_default())
        }
        Err(_) => (Vec::new(), String::new()),
    };
    if let Some(app) = &app {
        let _ = app.emit("gild://working", ());
    }
    let said = prompt(&agent.name, &role, &mentions, &context, &endpoint);
    let outcome = match &how {
        Wake::Command(command) => run_command(command, &said, LIMIT).await,
        Wake::Provider(runner) => match mentions.first() {
            None => Ok(String::new()),
            Some(first) => match tokio::time::timeout(LIMIT, run_provider(&core, &app, &agent, runner, &said, (&first.channel_id, first.thread.as_deref()))).await {
                Ok(r) => r.map(|()| String::new()),
                Err(_) => Err(format!("it was still running after {} minutes, so it was stopped", LIMIT.as_secs() / 60)),
            },
        },
    };
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
        let jobs: Vec<(Agent, Wake, Vec<Mention>)> = match core.lock() {
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
        for (agent, how, mentions) in jobs {
            if let Ok(mut r) = running.lock() {
                r.insert(agent.id.clone());
            }
            let (core, app, endpoint, running, signals) = (core.clone(), app.clone(), endpoint.clone(), running.clone(), signals.clone());
            tokio::spawn(async move {
                let id = agent.id.clone();
                wake(core, app, agent, how, mentions, endpoint).await;
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
        store.save_agent_profile(&agent.id, None, "You run student intake. Answer from the Intake SOP.").unwrap();
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
        // Its profile's instructions come with every wake.
        assert!(said.contains("Your instructions, from your profile in gild:\nYou run student intake. Answer from the Intake SOP.\n"), "{said}");
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

    /// A stand-in for Cloudflare's API, answering as the real one does:
    /// streamed chat completions (`data:` events with `delta.content` and
    /// `delta.tool_calls` in pieces, then `[DONE]`), every message's content
    /// a string, and model search with properties. First it asks for
    /// read_channel; then it writes its answer, a piece at a time — or, when
    /// the prompt says "plain", only writes.
    async fn workers_ai() -> (String, Arc<Mutex<Vec<Value>>>) {
        use axum::{routing::{get, post}, Json, Router};
        let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
        let log = seen.clone();
        let app = Router::new()
            .route(
                "/accounts/acct1/ai/v1/chat/completions",
                post(move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                    let log = log.clone();
                    async move {
                        use axum::response::IntoResponse;
                        assert_eq!(headers["authorization"], "Bearer test-token");
                        log.lock().unwrap().push(body.clone());
                        let messages = body["messages"].as_array().unwrap().clone();
                        if let Some(bad) = messages.iter().position(|m| !m["content"].is_string()) {
                            return (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "errors": [{ "message": format!("Type mismatch of '/messages/{bad}/content'") }] }))).into_response();
                        }
                        let prompt = messages[1]["content"].as_str().unwrap().to_string();
                        let delta = |d: Value| format!("data: {}\n\n", json!({ "object": "chat.completion.chunk", "choices": [{ "index": 0, "delta": d }] }));
                        let at = prompt.find("channel_id ").unwrap() + 11;
                        let channel: String = prompt[at..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                        let call = |name: &str, args: Value| delta(json!({ "tool_calls": [{ "index": 0, "id": format!("call_{}", messages.len()), "type": "function", "function": { "name": name, "arguments": args.to_string() } }] }));
                        // What it was asked: the newest marked line.
                        let latest = prompt.lines().rev().find(|l| l.starts_with("→ ")).unwrap_or("").to_string();
                        let events: Vec<String> = if latest.contains("post it") {
                            vec![call("post_message", json!({ "channel_id": channel, "text": "Posted with the tool." }))]
                        } else if latest.contains("repeat") {
                            vec![call("read_channel", json!({ "channel_id": channel }))]
                        } else if prompt.contains("plain") {
                            ["Plain ", "answer."].iter().map(|w| delta(json!({ "content": w }))).collect()
                        } else if messages.last().unwrap()["role"] == "tool" {
                            ["The forms ", "go in ", "the red ", "folder."].iter().map(|w| delta(json!({ "content": w }))).collect()
                        } else {
                            let at = prompt.find("channel_id ").unwrap() + 11;
                            let channel: String = prompt[at..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                            let args = json!({ "channel_id": channel }).to_string();
                            let (one, two) = args.split_at(args.len() / 2);
                            vec![
                                delta(json!({ "role": "assistant", "content": "" })),
                                delta(json!({ "tool_calls": [{ "index": 0, "id": "call_1", "type": "function", "function": { "name": "read_channel" } }] })),
                                delta(json!({ "tool_calls": [{ "index": 0, "function": { "arguments": one } }] })),
                                delta(json!({ "tool_calls": [{ "index": 0, "function": { "arguments": two } }] })),
                            ]
                        };
                        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, std::convert::Infallible>>(8);
                        tokio::spawn(async move {
                            for e in events {
                                let _ = tx.send(Ok(e)).await;
                                tokio::time::sleep(Duration::from_millis(250)).await;
                            }
                            let _ = tx.send(Ok("data: [DONE]\n\n".into())).await;
                        });
                        let body = axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
                        ([("content-type", "text/event-stream")], body).into_response()
                    }
                }),
            )
            .route(
                "/accounts/acct1/ai/models/search",
                get(|| async {
                    Json(json!({ "success": true, "errors": [], "result": [
                        { "name": "@cf/a/plain", "description": "No tools.", "properties": [{ "property_id": "context_window", "value": "8000" }] },
                        { "name": "@cf/b/tools", "description": "Calls tools.", "properties": [{ "property_id": "function_calling", "value": "true" }] }
                    ] }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, seen)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_cloud_agent_is_run_by_gild_through_the_same_tools() {
        let dir = std::env::temp_dir().join(format!("gild-wake-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let (base, seen) = workers_ai().await;
        let mut store = Store::open(&dir).unwrap();
        store.set_provider_base(&base);
        let ws = store.create_workspace("Front Desk").unwrap();
        let c = store.channels(&ws.id).unwrap()[0].clone();
        let (cloud, _) = store.create_agent("cloud").unwrap();
        store.set_access(&c.id, DEMO_NAME, &cloud.id, Some(Access::Write)).unwrap();
        let runner = Runner { provider: providers::WORKERS_AI.into(), model: "@cf/b/tools".into() };
        store.set_runner(&cloud.id, Some(&runner)).unwrap();
        assert!(store.creds(providers::WORKERS_AI).err().unwrap().contains("Settings › Providers"), "not set up yet");
        store.save_provider(providers::WORKERS_AI, "acct1", Some("test-token")).unwrap();
        let info = &store.providers().unwrap()[0];
        assert_eq!((info.account_id.as_deref(), info.has_token), (Some("acct1"), true), "the token itself is never handed out");
        store.set_tool(&cloud.id, "read_doc", false).unwrap();
        let core = Arc::new(Mutex::new(store));
        tokio::spawn(run(core.clone(), None, "http://127.0.0.1:4747/mcp".into()));

        core.lock().unwrap().post(&c.id, DEMO_NAME, "person", "@cloud where do the forms go?").unwrap();
        // The answer shows while it's being written: part of it, then all.
        let full = "The forms go in the red folder.";
        let mut partial = None;
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let now: Option<String> = core.lock().unwrap().messages(&c.id).unwrap().into_iter().find(|m| m.author == "cloud").map(|m| m.body);
            if let Some(b) = now.filter(|b| !b.is_empty() && b.len() < full.len()) {
                partial = Some(b);
                break;
            }
        }
        let partial = partial.expect("the reply was seen part-written");
        assert!(full.starts_with(&partial), "{partial:?}");
        let woken = wait_woken(&core, &c.id, 1).await;
        assert_eq!(woken, ["cloud: woken by Demo · replied (ok)"]);
        let store = core.lock().unwrap();
        let said: Vec<(String, String)> = store.messages(&c.id).unwrap().into_iter().map(|m| (m.author, m.body)).collect();
        assert_eq!(said.last().unwrap(), &("cloud".to_string(), full.to_string()), "the whole reply, once");
        assert_eq!(said.iter().filter(|(a, _)| a == "cloud").count(), 1, "the tool round wrote nothing, so left nothing");
        let calls: Vec<(String, bool)> = store.activity(&c.id).unwrap().into_iter().filter(|a| a.action != "woken").map(|a| (a.action, a.ok)).collect();
        assert_eq!(calls, [("read_channel".to_string(), true)], "its tool call, put together from pieces, ran and was logged like any agent's");
        drop(store);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "one round with the tool call, one to answer");
        assert_eq!((seen[0]["model"].as_str(), seen[0]["stream"].as_bool()), (Some("@cf/b/tools"), Some(true)));
        let offered: Vec<&str> = seen[0]["tools"].as_array().unwrap().iter().map(|t| t["function"]["name"].as_str().unwrap()).collect();
        assert!(offered.contains(&"post_message") && offered.contains(&"markdown_replace_section"));
        assert!(!offered.contains(&"read_doc"), "a tool turned off isn't offered");
        assert!(!offered.contains(&"wait_for_mentions"));
        assert_eq!(seen[1]["messages"].as_array().unwrap().last().unwrap()["tool_call_id"], "call_1");
        drop(seen);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_cloud_model_that_only_talks_gets_its_answer_posted() {
        let dir = std::env::temp_dir().join(format!("gild-wake-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let (base, _) = workers_ai().await;
        let mut store = Store::open(&dir).unwrap();
        store.set_provider_base(&base);
        let ws = store.create_workspace("Front Desk").unwrap();
        let c = store.channels(&ws.id).unwrap()[0].clone();
        let (cloud, _) = store.create_agent("cloud").unwrap();
        store.set_access(&c.id, DEMO_NAME, &cloud.id, Some(Access::Write)).unwrap();
        store.save_provider(providers::WORKERS_AI, "acct1", Some("test-token")).unwrap();
        store.set_runner(&cloud.id, Some(&Runner { provider: providers::WORKERS_AI.into(), model: "@cf/a/plain".into() })).unwrap();
        let root = store.post(&c.id, DEMO_NAME, "person", "Question time.").unwrap();
        let core = Arc::new(Mutex::new(store));
        tokio::spawn(run(core.clone(), None, "http://127.0.0.1:4747/mcp".into()));
        core.lock().unwrap().post_in(&c.id, Some(&root.id), DEMO_NAME, "person", "@cloud plain please").unwrap();
        assert_eq!(wait_woken(&core, &c.id, 1).await, ["cloud: woken by Demo · replied (ok)"]);
        let thread = core.lock().unwrap().thread(&c.id, &root.id).unwrap();
        assert_eq!((thread.last().unwrap().author.as_str(), thread.last().unwrap().body.as_str()), ("cloud", "Plain answer."), "posted where it was asked: the thread");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_provider_lists_its_models_tools_first() {
        let (base, _) = workers_ai().await;
        let creds = providers::Creds { base, account: "acct1".into(), token: "test-token".into() };
        let models = providers::models(&creds).await.unwrap();
        assert_eq!(models.iter().map(|m| (m.id.as_str(), m.tools, m.context_window)).collect::<Vec<_>>(), [("@cf/b/tools", true, None), ("@cf/a/plain", false, Some(8000))]);
    }

    async fn wait_woken(core: &Arc<Mutex<Store>>, channel: &str, n: usize) -> Vec<String> {
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let woken: Vec<String> = core
                .lock()
                .unwrap()
                .activity(channel)
                .unwrap()
                .into_iter()
                .filter(|a| a.action == "woken")
                .map(|a| format!("{}: {} ({})", a.agent, a.detail, if a.ok { "ok" } else { "failed" }))
                .collect();
            if woken.len() >= n {
                return woken;
            }
        }
        panic!("nobody was woken");
    }

    /// The real Workers AI, end to end: run by hand with
    /// `GILD_LIVE_CF=<file with ACCOUNT_ID= and TOKEN=> cargo test --lib live_ -- --ignored`.
    /// The token is read from the file and never printed.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn live_workers_ai_answers_a_mention() {
        let file = std::fs::read_to_string(std::env::var("GILD_LIVE_CF").expect("GILD_LIVE_CF names the token file")).unwrap();
        let field = |k: &str| file.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).unwrap().trim().to_string();
        let dir = std::env::temp_dir().join(format!("gild-wake-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir).unwrap();
        let ws = store.create_workspace("Front Desk").unwrap();
        let c = store.channels(&ws.id).unwrap()[0].clone();
        let (cloud, _) = store.create_agent("cloud").unwrap();
        store.set_access(&c.id, DEMO_NAME, &cloud.id, Some(Access::Write)).unwrap();
        store.save_provider(providers::WORKERS_AI, &field("ACCOUNT_ID"), Some(&field("TOKEN"))).unwrap();
        let models = providers::models(&store.creds(providers::WORKERS_AI).unwrap()).await.unwrap();
        println!("models: {} ({} can use tools)", models.len(), models.iter().filter(|m| m.tools).count());
        let model = std::env::var("GILD_LIVE_MODEL").unwrap_or_else(|_| "@cf/meta/llama-3.3-70b-instruct-fp8-fast".into());
        store.set_runner(&cloud.id, Some(&Runner { provider: providers::WORKERS_AI.into(), model })).unwrap();
        let core = Arc::new(Mutex::new(store));
        tokio::spawn(run(core.clone(), None, "http://127.0.0.1:4747/mcp".into()));
        core.lock().unwrap().post(&c.id, DEMO_NAME, "person", "@cloud please say hello to the team in this channel, in one short sentence.").unwrap();
        for _ in 0..240 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let store = core.lock().unwrap();
            let woken: Vec<String> = store.activity(&c.id).unwrap().into_iter().filter(|a| a.action == "woken").map(|a| format!("{} {}", a.detail, a.ok)).collect();
            if !woken.is_empty() {
                println!("woken: {woken:?}");
                let calls: Vec<String> = store.activity(&c.id).unwrap().into_iter().filter(|a| a.action != "woken").map(|a| format!("{} {}", a.action, a.ok)).collect();
                println!("calls: {calls:?}");
                println!("answer: {:?}", store.messages(&c.id).unwrap().last().map(|m| (m.author.clone(), m.body.clone())));
                assert!(woken[0].ends_with("replied true"), "{woken:?}");
                return;
            }
        }
        panic!("no answer within a minute");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_cloud_model_that_posts_its_answer_or_repeats_itself_is_done() {
        let dir = std::env::temp_dir().join(format!("gild-wake-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let (base, seen) = workers_ai().await;
        let mut store = Store::open(&dir).unwrap();
        store.set_provider_base(&base);
        let ws = store.create_workspace("Front Desk").unwrap();
        let c = store.channels(&ws.id).unwrap()[0].clone();
        let (cloud, _) = store.create_agent("cloud").unwrap();
        store.set_access(&c.id, DEMO_NAME, &cloud.id, Some(Access::Write)).unwrap();
        store.save_provider(providers::WORKERS_AI, "acct1", Some("test-token")).unwrap();
        store.set_runner(&cloud.id, Some(&Runner { provider: providers::WORKERS_AI.into(), model: "@cf/b/tools".into() })).unwrap();
        let core = Arc::new(Mutex::new(store));
        tokio::spawn(run(core.clone(), None, "http://127.0.0.1:4747/mcp".into()));

        // post_message where it was mentioned is its answer: no second round.
        core.lock().unwrap().post(&c.id, DEMO_NAME, "person", "@cloud post it").unwrap();
        wait_woken(&core, &c.id, 1).await;
        assert_eq!(seen.lock().unwrap().len(), 1);
        let posted: Vec<String> = core.lock().unwrap().messages(&c.id).unwrap().into_iter().filter(|m| m.author == "cloud").map(|m| m.body).collect();
        assert_eq!(posted, ["Posted with the tool."]);

        // The same call again isn't run again, and ends the run.
        core.lock().unwrap().post(&c.id, DEMO_NAME, "person", "@cloud repeat").unwrap();
        let woken = wait_woken(&core, &c.id, 2).await;
        assert!(woken.iter().all(|w| w.ends_with("(ok)")), "{woken:?}");
        assert_eq!(seen.lock().unwrap().len(), 3, "one more round, then it stopped");
        let reads = core.lock().unwrap().activity(&c.id).unwrap().into_iter().filter(|a| a.action == "read_channel").count();
        assert_eq!(reads, 1, "read once, not twice");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
