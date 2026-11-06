//! Cloud providers: agents that run on a provider's model instead of on this
//! computer. gild is the client: it calls the provider, runs the tool calls
//! the model asks for through the same code as the MCP endpoint, and the
//! provider never calls in.
//!
//! The first is Cloudflare Workers AI, through its OpenAI-compatible chat
//! completions (`/accounts/<id>/ai/v1/chat/completions`, `tools` and
//! `tool_calls` as OpenAI shapes them) and its model search, which marks the
//! models that can call tools.
//!
//! A provider's token lives in harar (Addis's vault) when it's installed,
//! else in the system keychain (macOS Keychain, Windows Credential
//! Manager), never in gild's database, where an agent on this computer could
//! read it.

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

pub const WORKERS_AI: &str = "workers-ai";
pub const WORKERS_AI_BASE: &str = "https://api.cloudflare.com/client/v4";

/// Where secrets are kept.
pub trait Secrets: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>, String>;
    fn set(&self, key: &str, value: Option<&str>) -> Result<(), String>;
}

/// The system keychain.
pub struct Keychain;

impl Secrets for Keychain {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        let entry = keyring::Entry::new("gild", key).map_err(|e| format!("the keychain: {e}"))?;
        match entry.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(format!("reading the keychain: {e}")),
        }
    }

    fn set(&self, key: &str, value: Option<&str>) -> Result<(), String> {
        let entry = keyring::Entry::new("gild", key).map_err(|e| format!("the keychain: {e}"))?;
        match value {
            Some(v) => entry.set_password(v).map_err(|e| format!("saving to the keychain: {e}")),
            None => match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(format!("removing from the keychain: {e}")),
            },
        }
    }
}

/// harar, Addis's vault: a per-user agent holding an encrypted vault, the
/// master key sealed to the Secure Enclave on a Mac. gild talks to it
/// through its CLI, its stable interface: `harar get <key>` answers JSON
/// `{"ok":true,"value":…}`, and `harar put <key> --secret-stdin` takes the
/// value on stdin, never on the command line.
pub struct Harar {
    pub bin: std::path::PathBuf,
}

impl Harar {
    /// harar where its installer (or cargo, or a package manager) puts it.
    pub fn find() -> Option<Harar> {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let mut places: Vec<std::path::PathBuf> = Vec::new();
        if let Some(h) = &home {
            places.push(h.join(".harar/bin/harar"));
            places.push(h.join(".cargo/bin/harar"));
        }
        places.push("/opt/homebrew/bin/harar".into());
        places.push("/usr/local/bin/harar".into());
        places.push("/usr/bin/harar".into());
        places.into_iter().find(|p| p.is_file()).map(|bin| Harar { bin })
    }

    /// How long a starting agent gets to answer.
    const START_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

    /// Make sure harar's agent is running: if `harar status` finds none,
    /// start `harar agent` in the background (it outlives gild, as harar
    /// means it to) and wait for it to answer. People shouldn't need a
    /// terminal for this.
    pub fn ensure_running(&self) -> Result<(), String> {
        if self.once(&["status"], None).is_ok() {
            return Ok(());
        }
        let mut agent = std::process::Command::new(&self.bin)
            .arg("agent")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("couldn't start harar: {e}"))?;
        let started = std::time::Instant::now();
        while started.elapsed() < Self::START_WAIT {
            std::thread::sleep(std::time::Duration::from_millis(150));
            if self.once(&["status"], None).is_ok() {
                return Ok(());
            }
            // It stopped instead: say why, in harar's words.
            if let Ok(Some(_)) = agent.try_wait() {
                let mut why = String::new();
                if let Some(mut err) = agent.stderr.take() {
                    use std::io::Read;
                    let _ = err.read_to_string(&mut why);
                }
                return Err(format!("harar's agent wouldn't start: {}", why.trim().lines().last().unwrap_or("it stopped at once")));
            }
        }
        Err("harar's agent didn't start in time. Try again in a moment.".into())
    }

    /// Whether an error means the agent isn't there: `status` says "no harar
    /// agent", but `get`/`put` report the socket itself — refused when an
    /// agent left its socket behind, missing when there was never one.
    fn agent_down(err: &str) -> bool {
        err.contains("no harar agent") || err.contains("Connection refused") || err.contains("No such file or directory")
    }

    /// A harar command, starting harar's agent first if it isn't running.
    fn run(&self, args: &[&str], input: Option<&str>) -> Result<String, String> {
        match self.once(args, input) {
            Err(e) if Self::agent_down(&e) => {
                self.ensure_running()?;
                self.once(args, input)
            }
            other => other,
        }
    }

    fn once(&self, args: &[&str], input: Option<&str>) -> Result<String, String> {
        use std::io::Write;
        let mut child = std::process::Command::new(&self.bin)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("couldn't run harar: {e}"))?;
        if let (Some(v), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin.write_all(v.as_bytes()).map_err(|e| format!("talking to harar: {e}"))?;
        }
        let out = child.wait_with_output().map_err(|e| format!("talking to harar: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    fn explain(err: String) -> String {
        if Self::agent_down(&err) {
            "harar's agent isn't running, and gild couldn't start it.".into()
        } else if err.contains("not_approved") {
            "harar didn't approve gild's request. Unlock harar (`harar unlock`) and try again.".into()
        } else {
            format!("harar: {err}")
        }
    }
}

impl Secrets for Harar {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        match self.run(&["get", key], None) {
            Ok(out) => {
                let v: Value = serde_json::from_str(out.trim()).map_err(|e| format!("harar answered something gild can't read: {e}"))?;
                Ok(v["value"].as_str().map(str::to_string).filter(|s| !s.is_empty()))
            }
            Err(e) if e.contains("not_found") => Ok(None),
            Err(e) => Err(Self::explain(e)),
        }
    }

    fn set(&self, key: &str, value: Option<&str>) -> Result<(), String> {
        // harar's CLI has no delete: an empty value is no token.
        self.run(&["put", key, "--secret-stdin"], Some(value.unwrap_or(""))).map(drop).map_err(Self::explain)
    }
}

/// Where gild keeps provider tokens: harar when it's installed (on every
/// platform), else the system keychain on macOS and Windows. A token saved
/// in the keychain before harar was installed moves into harar the first
/// time it's read. On Linux without harar there's nowhere safe, so saving
/// asks for harar.
pub struct Vault {
    pub harar: Option<Box<dyn Secrets>>,
    pub keychain: Option<Box<dyn Secrets>>,
}

impl Vault {
    pub fn for_this_computer() -> Vault {
        Vault {
            harar: Harar::find().map(|h| Box::new(h) as Box<dyn Secrets>),
            keychain: if cfg!(any(target_os = "macos", target_os = "windows")) { Some(Box::new(Keychain)) } else { None },
        }
    }

    /// Which one holds the tokens: "harar", "keychain" or "none".
    pub fn kind(&self) -> &'static str {
        if self.harar.is_some() { "harar" } else if self.keychain.is_some() { "keychain" } else { "none" }
    }
}

const NOWHERE: &str = "gild keeps provider tokens in harar, and harar isn't installed. Install it: curl -fsSL https://harar.addis.gg/install.sh | sh";

impl Secrets for Vault {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        match (&self.harar, &self.keychain) {
            (Some(h), k) => match h.get(key)? {
                Some(v) => Ok(Some(v)),
                None => match k.as_ref().map(|k| k.get(key)).transpose()?.flatten() {
                    Some(v) => {
                        h.set(key, Some(&v))?;
                        if let Some(k) = k {
                            k.set(key, None)?;
                        }
                        Ok(Some(v))
                    }
                    None => Ok(None),
                },
            },
            (None, Some(k)) => k.get(key),
            (None, None) => Ok(None),
        }
    }

    fn set(&self, key: &str, value: Option<&str>) -> Result<(), String> {
        match (&self.harar, &self.keychain) {
            (Some(h), k) => {
                h.set(key, value)?;
                if let Some(k) = k {
                    k.set(key, None)?;
                }
                Ok(())
            }
            (None, Some(k)) => k.set(key, value),
            (None, None) if value.is_none() => Ok(()),
            (None, None) => Err(NOWHERE.into()),
        }
    }
}

/// Secrets in memory: tests, and nothing else.
#[derive(Default)]
pub struct InMemory(Mutex<std::collections::HashMap<String, String>>);

impl Secrets for InMemory {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().map_err(|_| "poisoned")?.get(key).cloned())
    }
    fn set(&self, key: &str, value: Option<&str>) -> Result<(), String> {
        let mut m = self.0.lock().map_err(|_| "poisoned")?;
        match value {
            Some(v) => m.insert(key.to_string(), v.to_string()),
            None => m.remove(key),
        };
        Ok(())
    }
}

/// What it takes to call a provider.
#[derive(Clone)]
pub struct Creds {
    pub base: String,
    pub account: String,
    pub token: String,
}

/// A model a provider offers.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub description: String,
    /// It can call tools, which an agent needs to work in gild.
    pub tools: bool,
    pub context_window: Option<u64>,
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder().timeout(Duration::from_secs(120)).build().map_err(|e| e.to_string())
}

fn property<'a>(model: &'a Value, id: &str) -> Option<&'a Value> {
    model["properties"].as_array()?.iter().find(|p| p["property_id"] == id).map(|p| &p["value"])
}

/// The provider's text models, those that can use tools first.
pub async fn models(c: &Creds) -> Result<Vec<ModelInfo>, String> {
    let url = format!("{}/accounts/{}/ai/models/search?task=Text%20Generation&per_page=200", c.base, c.account);
    let res = client()?.get(url).bearer_auth(&c.token).send().await.map_err(|e| format!("Couldn't reach Workers AI: {e}"))?;
    let status = res.status();
    let body: Value = res.json().await.map_err(|e| format!("Workers AI answered something gild can't read: {e}"))?;
    if !status.is_success() || body["success"] == false {
        return Err(format!("Workers AI said no ({status}): {}", errors(&body)));
    }
    let mut out: Vec<ModelInfo> = body["result"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            Some(ModelInfo {
                id: m["name"].as_str()?.to_string(),
                description: m["description"].as_str().unwrap_or("").to_string(),
                tools: property(m, "function_calling").is_some_and(|v| v == "true" || v == true),
                context_window: property(m, "context_window").and_then(|v| v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_u64())),
            })
        })
        .collect();
    out.sort_by(|a, b| b.tools.cmp(&a.tools).then(a.id.cmp(&b.id)));
    Ok(out)
}

fn errors(body: &Value) -> String {
    let list: Vec<String> = body["errors"].as_array().into_iter().flatten().filter_map(|e| e["message"].as_str().map(str::to_string)).collect();
    if list.is_empty() { body.to_string().chars().take(200).collect() } else { list.join("; ") }
}

/// A tool call the model asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// One answer from the model: the assistant message to keep in the
/// conversation, and the tools it wants to call.
pub struct Turn {
    pub message: Value,
    pub tool_calls: Vec<ToolCall>,
}

/// One round of chat completions, streamed: `on_text` gets the answer's
/// text as it arrives; tool calls are put together from their pieces.
pub async fn chat_stream(c: &Creds, model: &str, messages: &[Value], tools: &[Value], mut on_text: impl FnMut(&str)) -> Result<Turn, String> {
    let url = format!("{}/accounts/{}/ai/v1/chat/completions", c.base, c.account);
    let mut body = json!({ "model": model, "messages": messages, "stream": true });
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    let mut res = client()?.post(url).bearer_auth(&c.token).json(&body).send().await.map_err(|e| format!("Couldn't reach Workers AI: {e}"))?;
    let status = res.status();
    if !status.is_success() {
        let answer: Value = res.json().await.unwrap_or(Value::Null);
        return Err(format!("Workers AI said no ({status}): {}", errors(&answer)));
    }
    let mut text = String::new();
    // Tool calls by their index: id, name, and the arguments' pieces.
    let mut calls: Vec<(String, String, String)> = Vec::new();
    let mut pending = String::new();
    let mut done = false;
    while !done {
        let Some(chunk) = res.chunk().await.map_err(|e| format!("Workers AI's answer broke off: {e}"))? else { break };
        pending.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = pending.find('\n') {
            let line: String = pending.drain(..=end).collect();
            let Some(data) = line.trim().strip_prefix("data:") else { continue };
            let data = data.trim();
            if data == "[DONE]" {
                done = true;
                break;
            }
            let Ok(event) = serde_json::from_str::<Value>(data) else { continue };
            let delta = &event["choices"][0]["delta"];
            if let Some(piece) = delta["content"].as_str().filter(|p| !p.is_empty()) {
                text.push_str(piece);
                on_text(piece);
            }
            for t in delta["tool_calls"].as_array().into_iter().flatten() {
                let i = t["index"].as_u64().unwrap_or(calls.len() as u64) as usize;
                while calls.len() <= i {
                    calls.push(Default::default());
                }
                if let Some(id) = t["id"].as_str() {
                    calls[i].0 = id.to_string();
                }
                if let Some(name) = t["function"]["name"].as_str() {
                    calls[i].1.push_str(name);
                }
                match &t["function"]["arguments"] {
                    Value::String(piece) => calls[i].2.push_str(piece),
                    Value::Object(_) => calls[i].2 = t["function"]["arguments"].to_string(),
                    _ => {}
                }
            }
        }
    }
    let tool_calls: Vec<ToolCall> = calls
        .iter()
        .filter(|(_, name, _)| !name.is_empty())
        .map(|(id, name, args)| ToolCall { id: id.clone(), name: name.clone(), arguments: serde_json::from_str(args).unwrap_or(Value::Object(Default::default())) })
        .collect();
    // What goes back into the conversation: Workers AI wants `content` as a
    // string even when the model only called tools.
    let mut message = json!({ "role": "assistant", "content": text });
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(calls
            .iter()
            .filter(|(_, name, _)| !name.is_empty())
            .map(|(id, name, args)| json!({ "id": id, "type": "function", "function": { "name": name, "arguments": if args.is_empty() { "{}" } else { args } } }))
            .collect::<Vec<_>>());
    }
    Ok(Turn { message, tool_calls })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the harar CLI, answering as harar does: `get` and
    /// `put --secret-stdin` over one file per key, while its agent "runs"
    /// (a file); without it, every command says there's no agent.
    /// `harar agent` starts it, or with `broken` fails as harar does when it
    /// can't open the vault.
    fn fake_harar(dir: &std::path::Path, running: bool, broken: bool) -> Harar {
        let bin = dir.join("harar");
        if running {
            std::fs::write(dir.join("running"), "").unwrap();
        }
        let agent = if broken { "echo 'Error: no master key at /x/master.key; create one with `harar key`' >&2; exit 1" } else { "touch \"$D/running\"; exit 0" };
        let body = format!(
            "#!/bin/sh\nD='{}'\n[ \"$1\" = agent ] && {{ {agent}; }}\n[ -f \"$D/running\" ] || {{ if [ \"$1\" = status ]; then echo \"Error: no harar agent at /x/agent.sock; start one with \\`harar agent\\`\" >&2; else echo 'Error: harar I/O error: Connection refused (os error 61)' >&2; fi; exit 1; }}\nk=$(printf %s \"$2\" | tr / _)\ncase $1 in\n status) echo '{{\"ok\":true}}' ;;\n get) [ -s \"$D/$k\" ] || {{ echo 'Error: harar refused: not_found' >&2; exit 1; }}; printf '{{\"ok\":true,\"value\":\"%s\"}}\\n' \"$(cat \"$D/$k\")\" ;;\n put) cat > \"$D/$k\"; echo '{{\"ok\":true}}' ;;\nesac\n",
            dir.display()
        );
        std::fs::write(&bin, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Harar { bin }
    }

    fn temp() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("gild-vault-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn tokens_live_in_harar_and_move_there_from_the_keychain() {
        let dir = temp();
        let keychain = InMemory::default();
        keychain.set("gild/workers-ai/token", Some("old-token")).unwrap();
        let vault = Vault { harar: Some(Box::new(fake_harar(&dir, true, false))), keychain: Some(Box::new(keychain)) };
        assert_eq!(vault.kind(), "harar");
        // The keychain's token moves into harar on first read…
        assert_eq!(vault.get("gild/workers-ai/token").unwrap().as_deref(), Some("old-token"));
        assert_eq!(std::fs::read_to_string(dir.join("gild_workers-ai_token")).unwrap(), "old-token");
        assert_eq!(vault.keychain.as_ref().unwrap().get("gild/workers-ai/token").unwrap(), None, "and leaves the keychain");
        // …and new ones go straight to harar.
        vault.set("gild/workers-ai/token", Some("new-token")).unwrap();
        assert_eq!(vault.get("gild/workers-ai/token").unwrap().as_deref(), Some("new-token"));
        vault.set("gild/workers-ai/token", None).unwrap();
        assert_eq!(vault.get("gild/workers-ai/token").unwrap(), None, "removed");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn gild_starts_harar_when_it_isn_t_running() {
        let dir = temp();
        let harar = fake_harar(&dir, false, false);
        assert!(harar.once(&["status"], None).is_err(), "not running to start with");
        let vault = Vault { harar: Some(Box::new(harar)), keychain: None };
        vault.set("gild/workers-ai/token", Some("t")).unwrap();
        assert!(dir.join("running").exists(), "gild started it");
        assert_eq!(vault.get("gild/workers-ai/token").unwrap().as_deref(), Some("t"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn harar_that_can_t_start_or_isn_t_installed_is_said_plainly() {
        let dir = temp();
        let vault = Vault { harar: Some(Box::new(fake_harar(&dir, false, true))), keychain: None };
        let e = vault.get("gild/workers-ai/token").unwrap_err();
        assert!(e.contains("harar's agent wouldn't start") && e.contains("create one with `harar key`"), "{e}");
        // Linux without harar: nowhere safe to keep it.
        let nowhere = Vault { harar: None, keychain: None };
        assert_eq!(nowhere.kind(), "none");
        assert!(nowhere.set("k", Some("v")).unwrap_err().contains("harar.addis.gg/install.sh"));
        assert_eq!(nowhere.get("k").unwrap(), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The real harar on this computer: run by hand with
    /// `cargo test --lib live_harar -- --ignored`. Starting the agent asks
    /// for nothing (it starts locked); this reads no secret.
    #[test]
    #[ignore]
    fn live_harar_is_started_when_it_isn_t_running() {
        let harar = Harar::find().expect("harar is installed");
        let before = harar.once(&["status"], None).is_ok();
        harar.ensure_running().unwrap();
        println!("was running: {before}; now: {}", harar.once(&["status"], None).unwrap().trim());
    }
}
