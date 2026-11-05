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

use crate::records::Access;
use crate::store::{Agent, Store};

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

fn tools() -> Vec<Tool> {
    let channel = json!({ "type": "string", "description": "The channel's id, from list_channels." });
    vec![
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
    ]
}

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
            "gild is a workspace where people and agents work together in channels. Start with list_channels: you see only the channels shared with you. Everything you do is recorded in the channel's audit log, and record changes are commits in the channel's history under your name. To answer a question about how things are done, use search_docs and cite the doc and section. You can't change a doc directly: propose_doc_edit, and a person approves it.",
        )
    }

    async fn list_tools(&self, _request: Option<PaginatedRequestParams>, _context: RequestContext<RoleServer>) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    async fn call_tool(&self, request: CallToolRequestParams, context: RequestContext<RoleServer>) -> Result<CallToolResponse, ErrorData> {
        let args = request.arguments.clone().unwrap_or_default();
        if request.name == "wait_for_mentions" {
            return self.wait_for_mentions(&args, &context).await;
        }
        let outcome = {
            let store = self.core.lock().map_err(|_| ErrorData::internal_error("the workspace database is unavailable", None))?;
            let agent = match bearer(&context).map(|t| store.agent_for_token(&t)).transpose() {
                Ok(Some(Some(agent))) => agent,
                Ok(_) => return Ok(no_token()),
                Err(e) => return Err(ErrorData::internal_error(e, None)),
            };
            let result = self.run(&store, &agent, &request.name, &args);
            let channel = args.get("channel_id").and_then(Value::as_str).unwrap_or("");
            let detail = match &result {
                Ok(_) => ["summary", "query", "title", "path", "text"]
                    .iter()
                    .find_map(|k| args.get(*k).and_then(Value::as_str))
                    .unwrap_or("")
                    .to_string(),
                Err(e) => e.clone(),
            };
            store.audit(&agent, &request.name, channel, &detail, result.is_ok()).map_err(|e| ErrorData::internal_error(e, None))?;
            result
        };
        match outcome {
            Ok((changed, answer)) => {
                if let (Some(app), Some(channel)) = (&self.app, changed) {
                    let _ = app.emit("gild://changed", channel);
                }
                let text = serde_json::to_string_pretty(&answer).unwrap_or_default();
                Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
            }
            Err(message) => Ok(CallToolResult::error(vec![ContentBlock::text(message)]).into()),
        }
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
}
