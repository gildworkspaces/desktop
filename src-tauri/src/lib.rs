mod clients;
mod export;
mod mcp;
mod records;
mod store;
mod wake;

use std::sync::{Arc, Mutex};

use serde::Serialize;
use std::collections::BTreeMap;

use clients::{ClientStatus, Clients, KINDS};
use records::{Access, Change, Doc, Record};
use store::{Agent, AgentAccess, AgentInfo, AuditEntry, Channel, ChannelData, ChannelDocs, Message, Store, Workspace, DEMO_NAME};
use tauri::{Manager, State};

/// The one store, opened when the app starts.
struct Core(Arc<Mutex<Store>>);

fn with<T>(core: &State<Core>, f: impl FnOnce(&Store) -> Result<T, String>) -> Result<T, String> {
    let store = core.0.lock().map_err(|_| "the workspace database is unavailable; restart gild".to_string())?;
    f(&store)
}

#[derive(Serialize)]
struct Account {
    name: &'static str,
    /// True until real accounts exist: everything is written as the demo account.
    demo: bool,
}

#[tauri::command]
fn account() -> Account {
    Account { name: DEMO_NAME, demo: true }
}

#[tauri::command]
fn workspaces(core: State<Core>) -> Result<Vec<Workspace>, String> {
    with(&core, |s| s.workspaces())
}

#[tauri::command]
fn create_workspace(core: State<Core>, name: String) -> Result<Workspace, String> {
    with(&core, |s| s.create_workspace(&name))
}

#[tauri::command]
fn channels(core: State<Core>, workspace_id: String) -> Result<Vec<Channel>, String> {
    with(&core, |s| s.channels(&workspace_id))
}

#[tauri::command]
fn create_channel(core: State<Core>, workspace_id: String, name: String, topic: String) -> Result<Channel, String> {
    with(&core, |s| s.create_channel(&workspace_id, &name, &topic))
}

#[tauri::command]
fn messages(core: State<Core>, channel_id: String) -> Result<Vec<Message>, String> {
    with(&core, |s| s.messages(&channel_id))
}

#[tauri::command]
fn send_message(core: State<Core>, channel_id: String, body: String) -> Result<Message, String> {
    with(&core, |s| s.post(&channel_id, DEMO_NAME, "person", &body))
}

#[tauri::command]
fn channel_data(core: State<Core>, channel_id: String) -> Result<ChannelData, String> {
    with(&core, |s| s.channel_data(&channel_id))
}

#[tauri::command]
fn set_view(core: State<Core>, channel_id: String, fields: Vec<String>, statuses: Vec<String>) -> Result<ChannelData, String> {
    with(&core, |s| s.set_view(&channel_id, DEMO_NAME, &fields, &statuses))
}

#[tauri::command]
fn add_record(core: State<Core>, channel_id: String, title: String, status: String, fields: BTreeMap<String, String>) -> Result<Record, String> {
    with(&core, |s| s.add_record(&channel_id, DEMO_NAME, &title, &status, fields))
}

#[tauri::command]
fn update_record(
    core: State<Core>,
    channel_id: String,
    record_id: String,
    title: String,
    status: String,
    fields: BTreeMap<String, String>,
) -> Result<Record, String> {
    with(&core, |s| s.update_record(&channel_id, DEMO_NAME, &record_id, &title, &status, fields))
}

#[tauri::command]
fn record_history(core: State<Core>, channel_id: String, record_id: String) -> Result<Vec<Change>, String> {
    with(&core, |s| s.record_history(&channel_id, &record_id))
}

#[tauri::command]
fn channel_docs(core: State<Core>, channel_id: String) -> Result<ChannelDocs, String> {
    with(&core, |s| s.channel_docs(&channel_id))
}

#[tauri::command]
fn create_doc(core: State<Core>, channel_id: String, title: String) -> Result<Doc, String> {
    with(&core, |s| s.create_doc(&channel_id, DEMO_NAME, &title))
}

#[tauri::command]
fn save_doc(core: State<Core>, channel_id: String, path: String, body: String) -> Result<Doc, String> {
    with(&core, |s| s.save_doc(&channel_id, DEMO_NAME, &path, &body))
}

#[tauri::command]
fn doc_history(core: State<Core>, channel_id: String, path: String) -> Result<Vec<Change>, String> {
    with(&core, |s| s.doc_history(&channel_id, &path))
}

#[tauri::command]
fn approve_proposal(core: State<Core>, channel_id: String, id: String) -> Result<ChannelDocs, String> {
    with(&core, |s| {
        s.approve_proposal(&channel_id, DEMO_NAME, &id)?;
        s.channel_docs(&channel_id)
    })
}

#[tauri::command]
fn reject_proposal(core: State<Core>, channel_id: String, id: String) -> Result<ChannelDocs, String> {
    with(&core, |s| {
        s.reject_proposal(&channel_id, DEMO_NAME, &id)?;
        s.channel_docs(&channel_id)
    })
}

#[derive(Serialize)]
struct NewAgent {
    agent: Agent,
    /// Shown once. Only its hash is kept.
    token: String,
    endpoint: String,
}

#[tauri::command]
fn agents(core: State<Core>) -> Result<Vec<AgentInfo>, String> {
    with(&core, |s| s.agent_infos())
}

/// What gild runs when the agent is mentioned (empty: nothing). Only here,
/// in the window: no agent can set what runs on this computer.
#[tauri::command]
fn set_wake(core: State<Core>, agent_id: String, command: String) -> Result<Vec<AgentInfo>, String> {
    with(&core, |s| {
        s.set_wake(&agent_id, &command)?;
        s.agent_infos()
    })
}

fn endpoint() -> String {
    format!("http://127.0.0.1:{}/mcp", mcp::PORT)
}

/// How many recent messages come with a mention.
#[tauri::command]
fn set_context(core: State<Core>, agent_id: String, messages: i64) -> Result<Vec<AgentInfo>, String> {
    with(&core, |s| {
        s.set_context(&agent_id, messages)?;
        s.agent_infos()
    })
}

#[tauri::command]
fn create_agent(core: State<Core>, name: String) -> Result<NewAgent, String> {
    let (agent, token) = with(&core, |s| s.create_agent(&name))?;
    Ok(NewAgent { agent, token, endpoint: endpoint() })
}

/// The MCP clients on this computer, and which already have the agent.
#[tauri::command]
async fn agent_clients(core: State<'_, Core>, agent_id: String) -> Result<Vec<ClientStatus>, String> {
    let agent = with(&core, |s| s.agent(&agent_id))?;
    Ok(Clients::for_this_computer().status(&agent.name).await)
}

#[derive(Serialize)]
struct Connected {
    /// The agent's new token, for a client gild can't set up itself.
    token: String,
    endpoint: String,
    clients: Vec<ClientStatus>,
    errors: Vec<String>,
    agents: Vec<AgentInfo>,
}

/// Set the agent up in the chosen clients with a new token, and take it out
/// of the others (their token stopped working).
#[tauri::command]
async fn connect_agent(core: State<'_, Core>, agent_id: String, clients: Vec<String>) -> Result<Connected, String> {
    let token = with(&core, |s| s.new_token(&agent_id))?;
    let agent = with(&core, |s| s.agent(&agent_id))?;
    let (here, endpoint) = (Clients::for_this_computer(), endpoint());
    let mut errors = Vec::new();
    for kind in KINDS {
        if clients.iter().any(|c| c == kind.id()) {
            if let Err(e) = here.connect(kind, &agent.name, &endpoint, &token).await {
                errors.push(e);
            }
        } else {
            here.disconnect(kind, &agent.name).await;
        }
    }
    let clients = here.status(&agent.name).await;
    // What wakes it follows the tool it's set up in, unless the person wrote
    // their own command.
    let ours: Vec<String> = KINDS.iter().map(|k| clients::wake_command(*k, &agent.name)).collect();
    let agents = with(&core, |s| {
        let current = s.agent_infos()?.into_iter().find(|a| a.agent.id == agent_id).and_then(|a| a.wake);
        if current.as_ref().is_none_or(|w| ours.contains(w)) {
            s.set_wake(&agent_id, clients.iter().find(|c| c.connected).map(|c| c.wake.as_str()).unwrap_or(""))?;
        }
        s.agent_infos()
    })?;
    Ok(Connected { token, endpoint, clients, errors, agents })
}

/// Remove the agent from gild and from the clients gild set it up in.
#[tauri::command]
async fn remove_agent(core: State<'_, Core>, agent_id: String) -> Result<Vec<AgentInfo>, String> {
    let agent = with(&core, |s| s.agent(&agent_id))?;
    let here = Clients::for_this_computer();
    for kind in KINDS {
        here.disconnect(kind, &agent.name).await;
    }
    with(&core, |s| {
        s.remove_agent(&agent_id, DEMO_NAME)?;
        s.agent_infos()
    })
}

#[tauri::command]
fn channel_access(core: State<Core>, channel_id: String) -> Result<Vec<AgentAccess>, String> {
    with(&core, |s| s.channel_access(&channel_id))
}

#[tauri::command]
fn set_access(core: State<Core>, channel_id: String, agent_id: String, access: Option<Access>) -> Result<Vec<AgentAccess>, String> {
    with(&core, |s| {
        s.set_access(&channel_id, DEMO_NAME, &agent_id, access)?;
        s.channel_access(&channel_id)
    })
}

#[tauri::command]
fn activity(core: State<Core>, channel_id: String) -> Result<Vec<AuditEntry>, String> {
    with(&core, |s| s.activity(&channel_id))
}

/// Where agents connect, or why they can't.
#[tauri::command]
fn mcp_status() -> Result<String, String> {
    mcp::STATUS.get().cloned().unwrap_or_else(|| Err("Starting the agent endpoint…".into()))
}

/// Export a workspace to Documents/gild exports and return the folder.
#[tauri::command]
fn export_workspace(app: tauri::AppHandle, core: State<Core>, workspace_id: String) -> Result<String, String> {
    // Documents, as the system names it; ~/Documents where it names none
    // (Linux without XDG user directories).
    let docs = match app.path().document_dir() {
        Ok(d) => d,
        Err(_) => app.path().home_dir().map_err(|e| format!("finding your home folder: {e}"))?.join("Documents"),
    };
    let stamp = {
        let ms = store::now();
        let secs = ms / 1000;
        format!("{} {:02}{:02}{:02}", export::date(ms), secs / 3600 % 24, secs / 60 % 60, secs % 60)
    };
    let out = with(&core, |s| export::export_workspace(s, &workspace_id, &docs.join("gild exports"), &stamp))?;
    Ok(out.display().to_string())
}

/// Make an export folder (one Export workspace wrote) a workspace again.
#[tauri::command]
fn import_workspace(core: State<Core>, folder: String) -> Result<Workspace, String> {
    with(&core, |s| export::import_workspace(s, std::path::Path::new(&folder)))
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let store = Store::open(&dir)?;
            store.seed_if_empty()?;
            let core = Arc::new(Mutex::new(store));
            mcp::serve(mcp::Gild { core: core.clone(), app: Some(app.handle().clone()) });
            tauri::async_runtime::spawn(wake::run(core.clone(), Some(app.handle().clone()), endpoint()));
            app.manage(Core(core));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            account,
            workspaces,
            create_workspace,
            channels,
            create_channel,
            messages,
            send_message,
            channel_data,
            set_view,
            add_record,
            update_record,
            record_history,
            channel_docs,
            create_doc,
            save_doc,
            doc_history,
            approve_proposal,
            reject_proposal,
            agents,
            set_wake,
            set_context,
            create_agent,
            agent_clients,
            connect_agent,
            remove_agent,
            channel_access,
            set_access,
            activity,
            mcp_status,
            export_workspace,
            import_workspace,
        ])
        .run(tauri::generate_context!())
        .expect("error while running gild desktop");
}
