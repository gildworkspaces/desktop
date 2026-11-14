//! The MCP endpoint agents use to work in gild: Streamable HTTP on
//! http://127.0.0.1:4747/mcp, loopback only (rmcp also refuses requests
//! whose Host isn't a loopback name, which stops DNS rebinding).
//!
//! Every call names its agent with `Authorization: Bearer <token>`. An agent
//! sees only the channels shared with it, can change only those it was given
//! write access to, and every call it makes is in the audit log. It goes
//! through the same store functions as the window: there is no second way in.

use std::sync::{Arc, Mutex, OnceLock};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::{session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{json, Map, Value};
use tauri::{AppHandle, Emitter};

use crate::addons::Registry;
use crate::records::Access;
use crate::store::{Agent, ChannelView, Store};
use crate::view::{Actor, Nav};

/// The pause after each move in the view, so a person watching can follow.
#[cfg(not(test))]
pub const PACE_MS: u64 = 300;
#[cfg(test)]
pub const PACE_MS: u64 = 1;

pub const PORT: u16 = 4747;

/// What the window shows about the endpoint: where it listens, or why not.
pub static STATUS: OnceLock<Result<String, String>> = OnceLock::new();

#[derive(Clone)]
pub struct Gild {
    pub core: Arc<Mutex<Store>>,
    pub app: Option<AppHandle>,
}

fn schema(v: Value) -> Arc<Map<String, Value>> {
    match v {
        Value::Object(m) => Arc::new(m),
        _ => Arc::new(Map::new()),
    }
}

pub(crate) fn tools() -> Vec<Tool> {
    let channel = json!({ "type": "string", "description": "The channel's id, from list_channels." });
    let mut tools = vec![
        Tool::new(
            "list_channels",
            "The channels shared with you, with the workspace each is in and whether you may only read it or also change it.",
            schema(json!({ "type": "object", "properties": {} })),
        ),
        Tool::new(
            "read_channel",
            "A channel's recent messages and, if it has one, its table: the columns, the statuses, and every record. A message with replies has a thread: read it with read_thread.",
            schema(json!({ "type": "object", "properties": {
                "channel_id": channel,
                "messages": { "type": "integer", "description": "How many recent messages to include (default 50, at most 200)." }
            }, "required": ["channel_id"] })),
        ),
        Tool::new(
            "read_thread",
            "A thread: the message it hangs off, then every reply, oldest first.",
            schema(json!({ "type": "object", "properties": {
                "channel_id": channel,
                "thread_id": { "type": "string", "description": "The id of the message the thread hangs off." }
            }, "required": ["channel_id", "thread_id"] })),
        ),
        Tool::new(
            "post_message",
            "Post a message in a channel, as yourself, or with thread_id as a reply in that message's thread. Needs write access.",
            schema(json!({ "type": "object", "properties": {
                "channel_id": channel, "text": { "type": "string" },
                "thread_id": { "type": "string", "description": "Reply in the thread of this message instead of the channel." }
            }, "required": ["channel_id", "text"] })),
        ),
        Tool::new(
            "add_record",
            "Add a record to a channel's table. `status` must be one of the table's statuses; `fields` maps column names to values. Needs write access.",
            schema(json!({ "type": "object", "properties": {
                "channel_id": channel,
                "title": { "type": "string" },
                "status": { "type": "string" },
                "fields": { "type": "object", "additionalProperties": { "type": "string" } }
            }, "required": ["channel_id", "title", "status"] })),
        ),
        Tool::new(
            "update_record",
            "Change a record. Leave out anything you aren't changing; in `fields`, an empty string clears a column. Needs write access.",
            schema(json!({ "type": "object", "properties": {
                "channel_id": channel,
                "record_id": { "type": "string" },
                "title": { "type": "string" },
                "status": { "type": "string" },
                "fields": { "type": "object", "additionalProperties": { "type": "string" } }
            }, "required": ["channel_id", "record_id"] })),
        ),
        Tool::new(
            "read_doc",
            "One of a channel's docs, as markdown. read_channel lists each channel's docs by path.",
            schema(json!({ "type": "object", "properties": {
                "channel_id": channel, "path": { "type": "string", "description": "The doc's path, such as docs/intake-sop.md." }
            }, "required": ["channel_id", "path"] })),
        ),
        Tool::new(
            "search_docs",
            "Search the docs in every channel shared with you. Each hit names the channel, the doc, and the section (heading) it's under, with the lines that matched: cite them when you answer.",
            schema(json!({ "type": "object", "properties": {
                "query": { "type": "string", "description": "A question or a few words, such as: where do the intake forms go?" }
            }, "required": ["query"] })),
        ),
        Tool::new(
            "propose_doc_edit",
            "Propose a doc's new text (all of it, as markdown), or a new doc. Nothing changes until a person in the channel approves it; they see your summary and a diff. Give `path` to edit a doc, or `title` for a new one. Needs write access.",
            schema(json!({ "type": "object", "properties": {
                "channel_id": channel,
                "path": { "type": "string", "description": "The doc to change." },
                "title": { "type": "string", "description": "A new doc's title, instead of `path`." },
                "text": { "type": "string", "description": "The doc's full new text, in markdown." },
                "summary": { "type": "string", "description": "What you changed and why, in a sentence: the person approving reads this." }
            }, "required": ["channel_id", "text", "summary"] })),
        ),
        Tool::new(
            "wait_for_mentions",
            "Wait until someone mentions you (@your-name) in a channel shared with you, and get those mentions: who said what, where. Returns at once if mentions are already waiting, or an empty list after `timeout_seconds`. Each mention is handed out once.",
            schema(json!({ "type": "object", "properties": {
                "timeout_seconds": { "type": "integer", "description": "How long to wait (default 60, at most 600)." }
            } })),
        ),
        Tool::new(
            "record_history",
            "Every change to one record, newest first: what changed, who changed it, and when.",
            schema(json!({ "type": "object", "properties": {
                "channel_id": channel, "record_id": { "type": "string" }
            }, "required": ["channel_id", "record_id"] })),
        ),
    ];
    // The channel's view: one shared screen, one controller at a time.
    let path = json!({ "type": "string", "description": "A path in the channel's files, e.g. docs/intake-sop.md." });
    let on_path = |name: &str, about: &str| {
        Tool::new(
            name.to_string(),
            about.to_string(),
            schema(json!({ "type": "object", "properties": { "channel_id": channel, "path": path }, "required": ["channel_id", "path"] })),
        )
    };
    let on_channel = |name: &str, about: &str| {
        Tool::new(name.to_string(), about.to_string(), schema(json!({ "type": "object", "properties": { "channel_id": channel }, "required": ["channel_id"] })))
    };
    tools.extend([
        on_channel("view_state", "What the channel's view shows now: its files, which folders are open, what's selected, the open file and its text, the operations it can be edited with, and who controls the view."),
        on_channel("take_view", "Take control of the channel's view, so you can move around it and edit files where people can watch. One controller at a time; a person always wins. Needs write access. Give it back with release_view when you're done."),
        on_channel("release_view", "Give the view back."),
        on_path("open_folder", "Open a folder in the view (you must hold the view)."),
        on_path("close_folder", "Close a folder in the view."),
        on_path("select", "Select a file or folder in the view."),
        on_path("open_file", "Open a file in the view. Edit it with its file type's operations (view_state lists them)."),
        on_channel("close_file", "Close the open file."),
    ]);
    // An add-on's general tools (Files: list_files, read_file, write_file,
    // move_file, delete_file, file_history), each in a channel.
    let registry = Registry::built_in();
    for t in registry.general_tools() {
        let mut properties = Map::new();
        properties.insert("channel_id".into(), channel.clone());
        properties.extend(t.op.params["properties"].as_object().cloned().unwrap_or_default());
        let mut required = vec![json!("channel_id")];
        required.extend(t.op.params["required"].as_array().cloned().unwrap_or_default());
        tools.push(Tool::new(t.op.name.clone(), t.op.description.clone(), schema(json!({ "type": "object", "properties": properties, "required": required }))));
    }
    // Every file type's operation is a tool.
    for t in registry.tools() {
        let mut params = t.op.params.clone();
        let props = params["properties"].as_object_mut().map(std::mem::take).unwrap_or_default();
        let mut properties = Map::new();
        properties.insert("channel_id".into(), channel.clone());
        properties.insert("path".into(), path.clone());
        properties.extend(props);
        let mut required = vec![json!("channel_id"), json!("path")];
        required.extend(params["required"].as_array().cloned().unwrap_or_default());
        params = json!({ "type": "object", "properties": properties, "required": required });
        tools.push(Tool::new(
            t.tool.clone(),
            format!("{} files: {} The file must be open in the view (open_file) while you hold it. One commit by you.", t.file_type.name, t.op.description),
            schema(params),
        ));
    }
    tools
}

/// A tool as the window lists it, for turning it on or off per agent.
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub group: String,
    /// The add-on it comes from, if any.
    pub addon: Option<String>,
}

/// Every tool, grouped the way people think of them.
pub fn catalog() -> Vec<ToolInfo> {
    let registry = Registry::built_in();
    tools()
        .into_iter()
        .map(|t| {
            let name = t.name.to_string();
            let from = registry
                .tool(&name)
                .map(|op| (format!("{} files", op.file_type.name), registry.addon_for(&format!("x{}", op.file_type.extensions[0])).map(|m| m.id.clone())))
                .or_else(|| registry.general_tool(&name).map(|g| (g.addon.name.clone(), Some(g.addon.id.clone()))));
            let group = match (&from, name.as_str()) {
                (Some((group, _)), _) => group.clone(),
                (None, "list_channels" | "read_channel" | "read_thread" | "post_message" | "wait_for_mentions") => "Channels and messages".into(),
                (None, "add_record" | "update_record" | "record_history") => "Table".into(),
                (None, "read_doc" | "search_docs" | "propose_doc_edit") => "Docs".into(),
                _ => "View".into(),
            };
            ToolInfo { name, description: t.description.as_deref().unwrap_or("").to_string(), group, addon: from.and_then(|(_, a)| a) }
        })
        .collect()
}

/// The view as an agent reads it.
fn view_json(v: &ChannelView) -> Value {
    json!({
        "controller": v.state.controller,
        "files": v.files.iter().map(|f| &f.path).collect::<Vec<_>>(),
        "open_folders": v.state.expanded,
        "selected": v.state.selected,
        "open_file": v.state.open,
        "file_type": v.file_type,
        "operations": v.operations,
        "text": v.text,
    })
}

/// Tools that move or change the view: paced so people can follow.
pub const PACED: [&str; 6] = ["open_folder", "close_folder", "select", "open_file", "close_file", "take_view"];

fn arg<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| format!("`{key}` is required."))
}

fn fields_arg(args: &Map<String, Value>) -> std::collections::BTreeMap<String, String> {
    args.get("fields")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))).collect())
        .unwrap_or_default()
}

fn bearer(context: &RequestContext<RoleServer>) -> Option<String> {
    let parts = context.extensions.get::<http::request::Parts>()?;
    let value = parts.headers.get(http::header::AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(|t| t.trim().to_string())
}

impl Gild {
    /// Run one tool for one agent. Returns what changed (a channel id) so the
    /// window can refresh, and the answer.
    fn run(&self, store: &Store, agent: &Agent, name: &str, args: &Map<String, Value>) -> Result<(Option<String>, Value), String> {
        match name {
            "list_channels" => {
                let channels: Vec<Value> = store
                    .agent_channels(agent)?
                    .into_iter()
                    .map(|c| json!({ "channel_id": c.channel.id, "name": c.channel.name, "topic": c.channel.topic, "workspace": c.workspace, "access": c.access }))
                    .collect();
                Ok((None, json!({ "you": agent.name, "channels": channels })))
            }
            "read_channel" => {
                let id = arg(args, "channel_id")?;
                store.require(agent, id, Access::Read)?;
                let channel = store.channel(id)?.ok_or("That channel no longer exists.")?;
                let n = args.get("messages").and_then(Value::as_u64).unwrap_or(50).min(200) as usize;
                let all = store.messages(id)?;
                let recent: Vec<Value> = all[all.len().saturating_sub(n)..]
                    .iter()
                    .map(message_json)
                    .collect();
                let data = store.channel_data(id)?;
                Ok((None, json!({
                    "channel": channel.name, "topic": channel.topic,
                    "access": store.access(&agent.id, id)?,
                    "messages": recent,
                    "table": data.view.map(|v| json!({ "columns": v.fields, "statuses": v.statuses })),
                    "records": data.records,
                    "docs": store.docs(id)?.into_iter().map(|d| json!({ "path": d.path, "title": d.title })).collect::<Vec<_>>(),
                })))
            }
            "read_doc" => {
                let id = arg(args, "channel_id")?;
                store.require(agent, id, Access::Read)?;
                let d = store.doc(id, arg(args, "path")?)?;
                Ok((None, json!({ "path": d.path, "title": d.title, "text": d.body })))
            }
            "search_docs" => {
                let ids: Vec<String> = store.agent_channels(agent)?.into_iter().map(|c| c.channel.id).collect();
                Ok((None, json!({ "hits": store.search_docs(&ids, arg(args, "query")?)? })))
            }
            "propose_doc_edit" => {
                let id = arg(args, "channel_id")?;
                store.require(agent, id, Access::Write)?;
                let path = args.get("path").and_then(Value::as_str);
                let title = args.get("title").and_then(Value::as_str);
                let p = store.propose_doc(id, &agent.name, path, title, arg(args, "text")?, arg(args, "summary")?)?;
                Ok((Some(id.to_string()), json!({ "proposal": p.id, "doc": p.path, "status": "waiting for a person to approve it" })))
            }
            "read_thread" => {
                let id = arg(args, "channel_id")?;
                store.require(agent, id, Access::Read)?;
                let thread = store.thread(id, arg(args, "thread_id")?)?;
                Ok((None, json!({ "messages": thread.iter().map(message_json).collect::<Vec<_>>() })))
            }
            "post_message" => {
                let id = arg(args, "channel_id")?;
                store.require(agent, id, Access::Write)?;
                let thread = args.get("thread_id").and_then(Value::as_str).filter(|t| !t.is_empty());
                let m = store.post_in(id, thread, &agent.name, "agent", arg(args, "text")?)?;
                Ok((Some(id.to_string()), json!({ "posted": m.id, "thread_id": m.thread })))
            }
            "add_record" => {
                let id = arg(args, "channel_id")?;
                store.require(agent, id, Access::Write)?;
                let r = store.add_record(id, &agent.name, arg(args, "title")?, arg(args, "status")?, fields_arg(args))?;
                Ok((Some(id.to_string()), json!(r)))
            }
            "update_record" => {
                let id = arg(args, "channel_id")?;
                store.require(agent, id, Access::Write)?;
                let record_id = arg(args, "record_id")?;
                let old = store
                    .channel_data(id)?
                    .records
                    .into_iter()
                    .find(|r| r.id == record_id)
                    .ok_or("No record with that id in this channel.")?;
                let mut fields = old.fields.clone();
                for (k, v) in fields_arg(args) {
                    fields.insert(k, v);
                }
                let title = args.get("title").and_then(Value::as_str).unwrap_or(&old.title);
                let status = args.get("status").and_then(Value::as_str).unwrap_or(&old.status);
                let r = store.update_record(id, &agent.name, record_id, title, status, fields)?;
                Ok((Some(id.to_string()), json!(r)))
            }
            "record_history" => {
                let id = arg(args, "channel_id")?;
                store.require(agent, id, Access::Read)?;
                Ok((None, json!(store.record_history(id, arg(args, "record_id")?)?)))
            }
            "view_state" => {
                let id = arg(args, "channel_id")?;
                store.require(agent, id, Access::Read)?;
                Ok((None, view_json(&store.view(id)?)))
            }
            "read_file" => {
                let id = arg(args, "channel_id")?;
                store.require(agent, id, Access::Read)?;
                let path = arg(args, "path")?;
                let text = store.file_text(id, path)?;
                Ok((None, json!({ "path": path, "text": text, "note": if text.is_none() { "not a text file gild can open" } else { "" } })))
            }
            // The Files add-on: general file tools, every write one commit
            // by the agent (the channel's access and the agent's tool
            // switches apply, as for every tool).
            "list_files" => {
                let id = arg(args, "channel_id")?;
                store.require(agent, id, Access::Read)?;
                Ok((None, json!(store.list_files(id)?)))
            }
            "file_history" => {
                let id = arg(args, "channel_id")?;
                store.require(agent, id, Access::Read)?;
                Ok((None, json!({ "path": arg(args, "path")?, "changes": store.file_history(id, arg(args, "path")?)? })))
            }
            "write_file" => {
                let id = arg(args, "channel_id")?;
                store.require(agent, id, Access::Write)?;
                let f = store.write_file(id, &agent.name, arg(args, "path")?, arg(args, "text")?.as_bytes().to_vec())?;
                Ok((Some(id.to_string()), json!({ "written": f.path, "size": f.size })))
            }
            "move_file" => {
                let id = arg(args, "channel_id")?;
                store.require(agent, id, Access::Write)?;
                let (from, to) = (arg(args, "path")?, arg(args, "to")?);
                store.move_file(id, &agent.name, from, to)?;
                Ok((Some(id.to_string()), json!({ "moved": from, "to": to })))
            }
            "delete_file" => {
                let id = arg(args, "channel_id")?;
                store.require(agent, id, Access::Write)?;
                let path = arg(args, "path")?;
                store.delete_file(id, &agent.name, path)?;
                Ok((Some(id.to_string()), json!({ "deleted": path })))
            }
            "take_view" => {
                let id = arg(args, "channel_id")?;
                Ok((Some(id.to_string()), view_json(&store.take_view(id, agent)?)))
            }
            "release_view" => {
                let id = arg(args, "channel_id")?;
                Ok((Some(id.to_string()), view_json(&store.release_view(id, agent)?)))
            }
            "open_folder" | "close_folder" | "select" | "open_file" | "close_file" => {
                let id = arg(args, "channel_id")?;
                store.require(agent, id, Access::Write)?;
                let nav = match name {
                    "open_folder" => Nav::Expand(arg(args, "path")?.to_string()),
                    "close_folder" => Nav::Collapse(arg(args, "path")?.to_string()),
                    "select" => Nav::Select(arg(args, "path")?.to_string()),
                    "open_file" => Nav::Open(arg(args, "path")?.to_string()),
                    _ => Nav::Close,
                };
                Ok((Some(id.to_string()), view_json(&store.drive(id, &Actor::Agent(agent.name.clone()), &nav)?)))
            }
            tool if store.addon_registry().tool(tool).is_some() => {
                let id = arg(args, "channel_id")?;
                store.require(agent, id, Access::Write)?;
                let op = store.addon_registry().tool(tool).map(|t| t.op.name.clone()).unwrap_or_default();
                let path = arg(args, "path")?;
                let v = store.edit_file(id, &Actor::Agent(agent.name.clone()), path, &op, &Value::Object(args.clone()))?;
                Ok((Some(id.to_string()), json!({ "edited": path, "text": v.text })))
            }
            other => Err(format!("There is no tool called {other}.")),
        }
    }
}

/// A message as agents see it.
fn message_json(m: &crate::store::Message) -> Value {
    let mut v = json!({ "id": m.id, "author": m.author, "kind": m.kind, "text": m.body, "at_ms": m.created_at });
    if m.replies > 0 {
        v["replies"] = json!(m.replies);
    }
    if let Some(t) = &m.thread {
        v["thread_id"] = json!(t);
    }
    v
}

fn no_token() -> CallToolResponse {
    CallToolResult::error(vec![ContentBlock::text(
        "This request has no valid agent token. Add an agent in gild and use its token: Authorization: Bearer <token>.",
    )])
    .into()
}

impl Gild {
    /// Block until the agent is mentioned (or the time is up) without
    /// holding the store: everyone else keeps working meanwhile.
    async fn wait_for_mentions(&self, args: &Map<String, Value>, context: &RequestContext<RoleServer>) -> Result<CallToolResponse, ErrorData> {
        let unavailable = || ErrorData::internal_error("the workspace database is unavailable", None);
        let (agent, signals) = {
            let store = self.core.lock().map_err(|_| unavailable())?;
            match bearer(context).map(|t| store.agent_for_token(&t)).transpose() {
                Ok(Some(Some(agent))) => (agent, store.signals()),
                Ok(_) => return Ok(no_token()),
                Err(e) => return Err(ErrorData::internal_error(e, None)),
            }
        };
        let seconds = args.get("timeout_seconds").and_then(Value::as_u64).unwrap_or(60).clamp(1, 600);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(seconds);
        loop {
            // Listen before looking, so a mention between the two isn't missed.
            let rung = signals.waiters.notified();
            tokio::pin!(rung);
            rung.as_mut().enable();
            let mentions = {
                let store = self.core.lock().map_err(|_| unavailable())?;
                let mentions = store.take_mentions(&agent).map_err(|e| ErrorData::internal_error(e, None))?;
                for m in &mentions {
                    store.audit(&agent, "wait_for_mentions", &m.channel_id, &format!("{}: {}", m.from, m.text), true).map_err(|e| ErrorData::internal_error(e, None))?;
                }
                mentions
            };
            if !mentions.is_empty() || tokio::time::Instant::now() >= deadline {
                let answer = json!({ "mentions": mentions.iter().map(|m| json!({
                    "channel_id": m.channel_id, "channel": m.channel, "workspace": m.workspace,
                    "from": m.from, "text": m.text, "message_id": m.message_id, "at_ms": m.at,
                })).collect::<Vec<_>>() });
                return Ok(CallToolResult::success(vec![ContentBlock::text(serde_json::to_string_pretty(&answer).unwrap_or_default())]).into());
            }
            let _ = tokio::time::timeout_at(deadline, rung).await;
        }
    }
}

impl ServerHandler for Gild {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("gild", env!("CARGO_PKG_VERSION")))
            .with_instructions(
            "gild is a workspace where people and agents work together in channels. Start with list_channels: you see only the channels shared with you. Everything you do is recorded in the channel's audit log, and record and file changes are commits in the channel's history under your name. Each channel is a folder of files: list_files, read_file, write_file, move_file, delete_file and file_history work on them where you have write access. To answer a question about how things are done, use search_docs and cite the doc and section. You can't change a doc directly: propose_doc_edit, and a person approves it.",
        )
    }

    async fn list_tools(&self, _request: Option<PaginatedRequestParams>, context: RequestContext<RoleServer>) -> Result<ListToolsResult, ErrorData> {
        // An agent sees only the tools it may use.
        let off = {
            let store = self.core.lock().map_err(|_| ErrorData::internal_error("the workspace database is unavailable", None))?;
            match bearer(&context).map(|t| store.agent_for_token(&t)).transpose() {
                Ok(Some(Some(agent))) => store.tools_off(&agent.id).map_err(|e| ErrorData::internal_error(e, None))?,
                _ => Vec::new(),
            }
        };
        Ok(ListToolsResult::with_all_items(tools().into_iter().filter(|t| !off.iter().any(|o| o == t.name.as_ref())).collect()))
    }

    async fn call_tool(&self, request: CallToolRequestParams, context: RequestContext<RoleServer>) -> Result<CallToolResponse, ErrorData> {
        let args = request.arguments.clone().unwrap_or_default();
        let agent = {
            let store = self.core.lock().map_err(|_| ErrorData::internal_error("the workspace database is unavailable", None))?;
            match bearer(&context).map(|t| store.agent_for_token(&t)).transpose() {
                Ok(Some(Some(agent))) => agent,
                Ok(_) => return Ok(no_token()),
                Err(e) => return Err(ErrorData::internal_error(e, None)),
            }
        };
        if request.name == "wait_for_mentions" {
            if let Err(why) = self.allowed(&agent, &request.name, &args) {
                return Ok(CallToolResult::error(vec![ContentBlock::text(why)]).into());
            }
            return self.wait_for_mentions(&args, &context).await;
        }
        match self.call_as(&agent, &request.name, &args).await {
            Ok(answer) => Ok(CallToolResult::success(vec![ContentBlock::text(serde_json::to_string_pretty(&answer).unwrap_or_default())]).into()),
            Err(message) => Ok(CallToolResult::error(vec![ContentBlock::text(message)]).into()),
        }
    }
}

impl Gild {
    /// Whether an agent may use a tool; a refusal is logged.
    fn allowed(&self, agent: &Agent, name: &str, args: &Map<String, Value>) -> Result<(), String> {
        let store = self.core.lock().map_err(|_| "the workspace database is unavailable".to_string())?;
        if store.tool_allowed(&agent.id, name)? {
            return Ok(());
        }
        let why = format!("{} isn't allowed to use {name} (Settings › Agents › {} › Tools).", agent.name, agent.name);
        store.audit(agent, name, args.get("channel_id").and_then(Value::as_str).unwrap_or(""), &why, false)?;
        Err(why)
    }

    /// One tool call as an agent: refused if the tool is off for it, run
    /// through the store, logged, paced when it moves the view, and the
    /// window told. The MCP endpoint and cloud providers both go through
    /// here, so an agent is the same agent wherever its model runs.
    pub async fn call_as(&self, agent: &Agent, name: &str, args: &Map<String, Value>) -> Result<Value, String> {
        self.allowed(agent, name, args)?;
        let outcome = {
            let store = self.core.lock().map_err(|_| "the workspace database is unavailable".to_string())?;
            let result = self.run(&store, agent, name, args);
            let channel = args.get("channel_id").and_then(Value::as_str).unwrap_or("");
            let detail = match &result {
                Ok(_) => ["summary", "query", "title", "path", "text"].iter().find_map(|k| args.get(*k).and_then(Value::as_str)).unwrap_or("").to_string(),
                Err(e) => e.clone(),
            };
            store.audit(agent, name, channel, &detail, result.is_ok())?;
            result
        };
        // Moves in the shared view, and edits, go at a pace people can follow.
        let paced = PACED.contains(&name) || Registry::built_in().tool(name).is_some();
        if outcome.is_ok() && paced {
            tokio::time::sleep(std::time::Duration::from_millis(PACE_MS)).await;
        }
        let (changed, answer) = outcome?;
        if let (Some(app), Some(channel)) = (&self.app, changed) {
            let _ = app.emit("gild://changed", channel);
        }
        Ok(answer)
    }
}

fn router(gild: Gild) -> axum::Router {
    let service: StreamableHttpService<Gild, LocalSessionManager> =
        StreamableHttpService::new(move || Ok(gild.clone()), Default::default(), StreamableHttpServerConfig::default());
    axum::Router::new().nest_service("/mcp", service)
}

/// Start the endpoint on the app's async runtime.
pub fn serve(gild: Gild) {
    tauri::async_runtime::spawn(async move {
        let address = format!("127.0.0.1:{PORT}");
        let listener = match tokio::net::TcpListener::bind(&address).await {
            Ok(l) => l,
            Err(e) => {
                let _ = STATUS.set(Err(format!("Agents can't connect: port {PORT} is in use ({e}).")));
                return;
            }
        };
        let _ = STATUS.set(Ok(format!("http://{address}/mcp")));
        let _ = axum::serve(listener, router(gild)).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::DEMO_NAME;

    /// A minimal MCP client over real HTTP: initialize, then call tools.
    struct Client {
        http: reqwest::Client,
        url: String,
        session: String,
        token: Option<String>,
        next: u64,
    }

    fn json_of(body: &str) -> Value {
        // The answer comes as JSON or as one server-sent event.
        let text = body.lines().filter_map(|l| l.strip_prefix("data:")).map(str::trim).find(|l| l.starts_with('{')).unwrap_or(body);
        serde_json::from_str(text).unwrap_or_else(|e| panic!("not JSON ({e}): {body}"))
    }

    impl Client {
        async fn post(&mut self, body: Value) -> (reqwest::header::HeaderMap, Value) {
            let mut req = self
                .http
                .post(&self.url)
                .header("Accept", "application/json, text/event-stream")
                .header("Content-Type", "application/json")
                .json(&body);
            if !self.session.is_empty() {
                req = req.header("Mcp-Session-Id", &self.session);
            }
            if let Some(t) = &self.token {
                req = req.header("Authorization", format!("Bearer {t}"));
            }
            let res = req.send().await.unwrap();
            let headers = res.headers().clone();
            let text = res.text().await.unwrap();
            (headers, if text.trim().is_empty() { Value::Null } else { json_of(&text) })
        }

        async fn connect(url: String, token: Option<String>) -> Client {
            let mut c = Client { http: reqwest::Client::new(), url, session: String::new(), token, next: 1 };
            let (headers, init) = c
                .post(json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
                    "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } } }))
                .await;
            assert!(init["result"]["capabilities"]["tools"].is_object(), "{init}");
            assert_eq!(init["result"]["serverInfo"]["name"], "gild");
            c.session = headers.get("mcp-session-id").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
            c.post(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await;
            c
        }

        /// (is_error, the text the tool answered)
        async fn call(&mut self, name: &str, args: Value) -> (bool, String) {
            self.next += 1;
            let (_, v) = self
                .post(json!({ "jsonrpc": "2.0", "id": self.next, "method": "tools/call", "params": { "name": name, "arguments": args } }))
                .await;
            let result = &v["result"];
            (result["isError"].as_bool().unwrap_or(false), result["content"][0]["text"].as_str().unwrap_or_default().to_string())
        }

        async fn list_tools(&mut self) -> Vec<String> {
            self.next += 1;
            let (_, v) = self.post(json!({ "jsonrpc": "2.0", "id": self.next, "method": "tools/list", "params": {} })).await;
            v["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn agents_work_through_the_endpoint_and_only_where_allowed() {
        let dir = std::env::temp_dir().join(format!("gild-mcp-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir).unwrap();
        let ws = store.create_workspace("Front Desk").unwrap();
        let general = store.channels(&ws.id).unwrap()[0].clone();
        let intake = store.create_channel(&ws.id, "student-intake", "").unwrap();
        let hidden = store.create_channel(&ws.id, "payroll", "").unwrap();
        store.set_view(&intake.id, DEMO_NAME, &["Program".into()], &["New".into(), "Enrolled".into()]).unwrap();
        let (agent, token) = store.create_agent("intake").unwrap();
        store.set_access(&intake.id, DEMO_NAME, &agent.id, Some(Access::Write)).unwrap();
        store.set_access(&general.id, DEMO_NAME, &agent.id, Some(Access::Read)).unwrap();
        let sop = store.create_doc(&intake.id, DEMO_NAME, "Intake SOP").unwrap();
        store.save_doc(&intake.id, DEMO_NAME, &sop.path, "# Intake SOP\n\n## Step 2: Forms\n\nIntake forms go in the blue folder.\n").unwrap();
        let secret = store.create_doc(&hidden.id, DEMO_NAME, "Payroll").unwrap();
        store.save_doc(&hidden.id, DEMO_NAME, &secret.path, "# Payroll\n\nIntake forms for payroll go to the accountant.\n").unwrap();
        let core = Arc::new(Mutex::new(store));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let app = router(Gild { core: core.clone(), app: None });
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        // No token: nothing.
        let mut anon = Client::connect(url.clone(), None).await;
        let (err, text) = anon.call("list_channels", json!({})).await;
        assert!(err && text.contains("no valid agent token"), "{text}");
        let mut wrong = Client::connect(url.clone(), Some("gild_nope".into())).await;
        assert!(wrong.call("list_channels", json!({})).await.0);

        let mut c = Client::connect(url, Some(token)).await;
        let (err, text) = c.call("list_channels", json!({})).await;
        assert!(!err, "{text}");
        let listed: Value = serde_json::from_str(&text).unwrap();
        let names: Vec<&str> = listed["channels"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["general", "student-intake"], "payroll was never shared");

        // Writes where allowed land in the store under the agent's name.
        let (err, text) = c.call("add_record", json!({ "channel_id": intake.id, "title": "Jordan Lee", "status": "New", "fields": { "Program": "Web Dev" } })).await;
        assert!(!err, "{text}");
        let id = serde_json::from_str::<Value>(&text).unwrap()["id"].as_str().unwrap().to_string();
        let (err, text) = c.call("update_record", json!({ "channel_id": intake.id, "record_id": id, "status": "Enrolled" })).await;
        assert!(!err, "{text}");
        assert!(!c.call("post_message", json!({ "channel_id": intake.id, "text": "Jordan is enrolled." })).await.0);

        // Refused where not allowed, and the refusal doesn't reveal the channel.
        let (err, text) = c.call("post_message", json!({ "channel_id": general.id, "text": "hi" })).await;
        assert!(err && text.contains("not change"), "{text}");
        let (err, text) = c.call("read_channel", json!({ "channel_id": hidden.id })).await;
        assert!(err && text.contains("No channel with that id"), "{text}");

        // Docs: search answers from the channels shared with it, never others.
        let (err, text) = c.call("search_docs", json!({ "query": "Where do the intake forms go?" })).await;
        assert!(!err, "{text}");
        let hits: Value = serde_json::from_str(&text).unwrap();
        let found: Vec<(&str, &str)> = hits["hits"].as_array().unwrap().iter().map(|h| (h["title"].as_str().unwrap(), h["section"].as_str().unwrap())).collect();
        assert_eq!(found, [("Intake SOP", "Step 2: Forms")], "payroll's doc was never shared");
        let (err, text) = c.call("read_doc", json!({ "channel_id": hidden.id, "path": secret.path })).await;
        assert!(err && text.contains("No channel with that id"), "{text}");
        // An edit is a proposal: the doc doesn't change.
        let (err, text) = c
            .call("propose_doc_edit", json!({ "channel_id": intake.id, "path": sop.path, "text": "# Intake SOP\n\nForms go in the red folder.\n", "summary": "The folder is red now" }))
            .await;
        assert!(!err && text.contains("waiting for a person"), "{text}");
        let (err, text) = c.call("propose_doc_edit", json!({ "channel_id": general.id, "title": "Hi", "text": "# Hi\n", "summary": "hi" })).await;
        assert!(err && text.contains("not change"), "read access can't propose: {text}");
        let (_, text) = c.call("read_doc", json!({ "channel_id": intake.id, "path": sop.path })).await;
        assert!(text.contains("blue folder"), "{text}");

        // Mentions: nothing waiting → empty after the timeout; a mention while
        // it waits → returned as soon as it's posted.
        let started = std::time::Instant::now();
        let (err, text) = c.call("wait_for_mentions", json!({ "timeout_seconds": 1 })).await;
        assert!(!err && text.contains("\"mentions\": []") && started.elapsed() >= std::time::Duration::from_secs(1), "{text}");
        let poster = core.clone();
        let channel = intake.id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            poster.lock().unwrap().post(&channel, DEMO_NAME, "person", "@intake can you check Jordan's forms?").unwrap();
        });
        let started = std::time::Instant::now();
        let (err, text) = c.call("wait_for_mentions", json!({ "timeout_seconds": 30 })).await;
        assert!(!err && text.contains("can you check Jordan's forms?") && started.elapsed() < std::time::Duration::from_secs(5), "{text}");
        let (_, text) = c.call("wait_for_mentions", json!({ "timeout_seconds": 1 })).await;
        assert!(text.contains("\"mentions\": []"), "handed out once: {text}");

        let store = core.lock().unwrap();
        let record = &store.channel_data(&intake.id).unwrap().records[0];
        assert_eq!((record.status.as_str(), record.fields["Program"].as_str()), ("Enrolled", "Web Dev"), "an update keeps what it didn't name");
        let history: Vec<String> = store.record_history(&intake.id, &id).unwrap().into_iter().map(|h| format!("{}: {}", h.author, h.message)).collect();
        assert_eq!(history, ["intake: Jordan Lee: New → Enrolled", "intake: Add Jordan Lee"]);
        let posted = store.messages(&intake.id).unwrap();
        assert_eq!((posted[0].author.as_str(), posted[0].kind.as_str()), ("intake", "agent"));
        let audit: Vec<(String, bool)> = store.activity(&intake.id).unwrap().into_iter().map(|a| (a.action, a.ok)).collect();
        assert_eq!(
            audit,
            [("wait_for_mentions", true), ("read_doc", true), ("propose_doc_edit", true), ("post_message", true), ("update_record", true), ("add_record", true)].map(|(a, ok)| (a.to_string(), ok))
        );
        let refused: Vec<bool> = store.activity(&general.id).unwrap().into_iter().map(|a| a.ok).collect();
        assert_eq!(refused, [false, false], "refusals are in the log too");
        let waiting = store.proposals(&intake.id).unwrap();
        assert_eq!((waiting.len(), waiting[0].agent.as_str(), waiting[0].summary.as_str()), (1, "intake", "The folder is red now"));
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_catalog_groups_every_tool() {
        let c = catalog();
        assert_eq!(c.len(), tools().len());
        let group = |n: &str| c.iter().find(|t| t.name == n).map(|t| (t.group.clone(), t.addon.clone())).unwrap();
        assert_eq!(group("post_message"), ("Channels and messages".into(), None));
        assert_eq!(group("open_file"), ("View".into(), None));
        assert_eq!(group("markdown_replace_section"), ("Markdown files".into(), Some("gild.markdown".into())));
        for t in ["list_files", "read_file", "write_file", "move_file", "delete_file", "file_history"] {
            assert_eq!(group(t), ("Files".into(), Some("gild.files".into())), "{t}");
        }
        let names: Vec<&str> = c.iter().map(|t| t.name.as_str()).collect();
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(names.len(), unique.len(), "every tool once: {names:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn agents_manage_files_where_they_may_and_every_write_is_theirs() {
        let dir = std::env::temp_dir().join(format!("gild-mcp-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir).unwrap();
        let ws = store.create_workspace("Front Desk").unwrap();
        let intake = store.create_channel(&ws.id, "student-intake", "").unwrap();
        let general = store.channels(&ws.id).unwrap()[0].clone();
        let (agent, token) = store.create_agent("intake").unwrap();
        store.set_access(&intake.id, DEMO_NAME, &agent.id, Some(Access::Write)).unwrap();
        store.set_access(&general.id, DEMO_NAME, &agent.id, Some(Access::Read)).unwrap();
        store.write_file(&general.id, DEMO_NAME, "forms/consent.txt", b"Sign here.".to_vec()).unwrap();
        let folder = store.repo(&intake.id).unwrap();
        let core = Arc::new(Mutex::new(store));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let app = router(Gild { core: core.clone(), app: None });
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut c = Client::connect(url, Some(token)).await;
        let (ch, ro) = (intake.id.as_str(), general.id.as_str());

        // The tools are offered, with the channel and their own arguments.
        let listed = c.list_tools().await;
        for t in ["list_files", "read_file", "write_file", "move_file", "delete_file", "file_history"] {
            assert!(listed.contains(&t.to_string()), "{t}");
        }
        let (err, text) = c.call("write_file", json!({ "channel_id": ch, "path": "notes/first.md", "text": "# First\n\nHello.\n" })).await;
        assert!(!err && text.contains("\"written\": \"notes/first.md\""), "{text}");
        assert_eq!(std::fs::read_to_string(folder.join("notes/first.md")).unwrap(), "# First\n\nHello.\n");
        let (err, text) = c.call("list_files", json!({ "channel_id": ch })).await;
        assert!(!err, "{text}");
        let list: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(list["files"][0]["path"], "notes/first.md");
        assert_eq!(list["folders"], json!(["notes"]));
        let (err, text) = c.call("read_file", json!({ "channel_id": ch, "path": "notes/first.md" })).await;
        assert!(!err && text.contains("Hello."), "{text}");
        assert!(!c.call("move_file", json!({ "channel_id": ch, "path": "notes/first.md", "to": "notes/renamed.md" })).await.0);
        let (err, text) = c.call("file_history", json!({ "channel_id": ch, "path": "notes/renamed.md" })).await;
        assert!(!err && text.contains("Move notes/first.md to notes/renamed.md"), "{text}");
        assert!(!c.call("delete_file", json!({ "channel_id": ch, "path": "notes" })).await.0);
        assert!(!folder.join("notes").exists());

        // Hostile paths, gild's own files: refused, nothing written.
        for bad in ["../../escape.md", ".gild/acl.json", ".git/config", "/tmp/x"] {
            let (err, text) = c.call("write_file", json!({ "channel_id": ch, "path": bad, "text": "x" })).await;
            assert!(err, "{bad}: {text}");
        }
        assert!(!dir.join("escape.md").exists() && !folder.parent().unwrap().join("escape.md").exists());
        // Read access reads, never writes; a channel it can't see doesn't exist.
        let (err, text) = c.call("read_file", json!({ "channel_id": ro, "path": "forms/consent.txt" })).await;
        assert!(!err && text.contains("Sign here."), "{text}");
        let (err, text) = c.call("write_file", json!({ "channel_id": ro, "path": "forms/x.txt", "text": "x" })).await;
        assert!(err && text.contains("not change"), "{text}");
        let (err, text) = c.call("delete_file", json!({ "channel_id": ro, "path": "forms/consent.txt" })).await;
        assert!(err && text.contains("not change"), "{text}");
        // The switch: off, and the tool is gone and refused.
        core.lock().unwrap().set_tool(&agent.id, "delete_file", false).unwrap();
        assert!(!c.list_tools().await.contains(&"delete_file".to_string()));
        let (err, text) = c.call("delete_file", json!({ "channel_id": ch, "path": "x" })).await;
        assert!(err && text.contains("isn't allowed to use delete_file"), "{text}");

        let store = core.lock().unwrap();
        assert_eq!(store.file_text(&general.id, "forms/consent.txt").unwrap().as_deref(), Some("Sign here."));
        let log: Vec<(String, bool)> = store.activity(ch).unwrap().into_iter().map(|a| (a.action, a.ok)).collect();
        assert_eq!(
            log,
            [("delete_file", false), ("write_file", false), ("write_file", false), ("write_file", false), ("write_file", false), ("delete_file", true), ("file_history", true), ("move_file", true), ("read_file", true), ("list_files", true), ("write_file", true)]
                .map(|(a, ok)| (a.to_string(), ok)),
            "every call, allowed or refused, is in Activity"
        );
        let history = store.file_history(ch, "notes").unwrap();
        assert_eq!(history.iter().map(|h| format!("{}: {}", h.author, h.message)).collect::<Vec<_>>(), ["intake: Delete notes", "intake: Move notes/first.md to notes/renamed.md", "intake: Add notes/first.md"]);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn every_add_on_operation_is_a_tool_with_its_own_arguments() {
        let all = tools();
        let t = all.iter().find(|t| t.name == "markdown_replace_section").expect("generated from the markdown add-on");
        let props: Vec<&String> = t.input_schema["properties"].as_object().unwrap().keys().collect();
        assert_eq!(props, ["channel_id", "heading", "path", "text"]);
        assert_eq!(t.input_schema["required"], json!(["channel_id", "path", "heading", "text"]));
        for name in ["view_state", "read_file", "take_view", "release_view", "open_folder", "close_folder", "select", "open_file", "close_file"] {
            assert!(all.iter().any(|t| t.name == name), "{name}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn agents_drive_the_shared_view_and_a_person_takes_it_back() {
        let dir = std::env::temp_dir().join(format!("gild-mcp-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir).unwrap();
        let ws = store.create_workspace("Front Desk").unwrap();
        let intake = store.channels(&ws.id).unwrap()[0].clone();
        let (agent, token) = store.create_agent("intake").unwrap();
        store.set_access(&intake.id, DEMO_NAME, &agent.id, Some(Access::Write)).unwrap();
        let sop = store.create_doc(&intake.id, DEMO_NAME, "Intake SOP").unwrap();
        store.save_doc(&intake.id, DEMO_NAME, &sop.path, "# Intake SOP\n\n## Step 2: Forms\n\nIntake forms go in the blue folder.\n").unwrap();
        let core = Arc::new(Mutex::new(store));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let app = router(Gild { core: core.clone(), app: None });
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut c = Client::connect(url, Some(token)).await;
        let ch = intake.id.as_str();

        let (err, text) = c.call("view_state", json!({ "channel_id": ch })).await;
        assert!(!err && text.contains("\"kind\": \"none\"") && text.contains(&sop.path), "{text}");
        let (err, text) = c.call("open_folder", json!({ "channel_id": ch, "path": "docs" })).await;
        assert!(err && text.contains("take_view"), "{text}");
        assert!(!c.call("take_view", json!({ "channel_id": ch })).await.0);
        assert!(!c.call("open_folder", json!({ "channel_id": ch, "path": "docs" })).await.0);
        let (err, text) = c.call("open_file", json!({ "channel_id": ch, "path": sop.path })).await;
        assert!(!err && text.contains("replace_section") && text.contains("blue folder"), "{text}");
        let (err, text) = c
            .call("markdown_replace_section", json!({ "channel_id": ch, "path": sop.path, "heading": "Step 2: Forms", "text": "Intake forms go in the red folder." }))
            .await;
        assert!(!err && text.contains("red folder"), "{text}");
        // The window sees the same view: the agent in control, the file open, the edit in it.
        {
            let store = core.lock().unwrap();
            let v = store.view(ch).unwrap();
            assert!(matches!(&v.state.controller, crate::view::Controller::Agent { name, .. } if name == "intake"));
            assert_eq!(v.state.open.as_deref(), Some(sop.path.as_str()));
            assert!(v.text.unwrap().contains("red folder"));
            // A person takes over.
            store.take_over(ch, DEMO_NAME).unwrap();
        }
        let (err, text) = c.call("markdown_append", json!({ "channel_id": ch, "path": sop.path, "text": "More." })).await;
        assert!(err && text.contains("Demo took over"), "{text}");
        let (err, text) = c.call("take_view", json!({ "channel_id": ch })).await;
        assert!(err && text.contains("Demo is using the view"), "{text}");
        // A tool turned off for the agent is gone from its list and refused.
        core.lock().unwrap().set_tool(&agent.id, "markdown_append", false).unwrap();
        let listed = c.list_tools().await;
        assert!(!listed.contains(&"markdown_append".to_string()) && listed.contains(&"markdown_replace_section".to_string()));
        let (err, text) = c.call("markdown_append", json!({ "channel_id": ch, "path": sop.path, "text": "x" })).await;
        assert_eq!((err, text.as_str()), (true, "intake isn't allowed to use markdown_append (Settings › Agents › intake › Tools)."));
        let store = core.lock().unwrap();
        let log: Vec<(String, bool)> = store.activity(ch).unwrap().into_iter().map(|a| (a.action, a.ok)).collect();
        assert_eq!(
            log,
            [("markdown_append", false), ("take_view", false), ("markdown_append", false), ("markdown_replace_section", true), ("open_file", true), ("open_folder", true), ("take_view", true), ("open_folder", false), ("view_state", true)]
                .map(|(a, ok)| (a.to_string(), ok))
        );
        assert_eq!(store.doc_history(ch, &sop.path).unwrap()[0].author, "intake");
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
