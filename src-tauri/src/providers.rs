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
//! A provider's token lives in the system keychain (macOS Keychain, Windows
//! Credential Manager), never in gild's database, where an agent on this
//! computer could read it.

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
/// conversation, what it said, and the tools it wants to call.
pub struct Turn {
    pub message: Value,
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

/// One round of chat completions.
pub async fn chat(c: &Creds, model: &str, messages: &[Value], tools: &[Value]) -> Result<Turn, String> {
    let url = format!("{}/accounts/{}/ai/v1/chat/completions", c.base, c.account);
    let mut body = json!({ "model": model, "messages": messages });
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    let res = client()?.post(url).bearer_auth(&c.token).json(&body).send().await.map_err(|e| format!("Couldn't reach Workers AI: {e}"))?;
    let status = res.status();
    let answer: Value = res.json().await.map_err(|e| format!("Workers AI answered something gild can't read: {e}"))?;
    if !status.is_success() {
        return Err(format!("Workers AI said no ({status}): {}", errors(&answer)));
    }
    let message = answer["choices"][0]["message"].clone();
    if message.is_null() {
        return Err(format!("Workers AI sent no answer: {}", errors(&answer)));
    }
    let tool_calls = message["tool_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let f = &t["function"];
            let arguments = match &f["arguments"] {
                Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Object(Default::default())),
                Value::Object(_) => f["arguments"].clone(),
                _ => Value::Object(Default::default()),
            };
            Some(ToolCall { id: t["id"].as_str().unwrap_or("").to_string(), name: f["name"].as_str()?.to_string(), arguments })
        })
        .collect();
    let content = message["content"].as_str().map(str::to_string).filter(|s| !s.trim().is_empty());
    // What goes back into the conversation: Workers AI wants `content` as a
    // string even when the model only called tools (it answers with null).
    let mut message = message;
    message["content"] = json!(content.clone().unwrap_or_default());
    Ok(Turn { message, content, tool_calls })
}
