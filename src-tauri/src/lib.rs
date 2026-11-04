mod export;
mod mcp;
mod records;
mod store;

use std::sync::{Arc, Mutex};

use serde::Serialize;
use std::collections::BTreeMap;

use records::{Access, Change, Doc, Record};
use store::{Agent, AgentAccess, AuditEntry, Channel, ChannelData, ChannelDocs, Message, Store, Workspace, DEMO_NAME};
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
fn agents(core: State<Core>) -> Result<Vec<Agent>, String> {
    with(&core, |s| s.agents())
}

#[tauri::command]
fn create_agent(core: State<Core>, name: String) -> Result<NewAgent, String> {
    let (agent, token) = with(&core, |s| s.create_agent(&name))?;
    Ok(NewAgent { agent, token, endpoint: format!("http://127.0.0.1:{}/mcp", mcp::PORT) })
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

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let store = Store::open(&dir)?;
            store.seed_if_empty()?;
            let core = Arc::new(Mutex::new(store));
            mcp::serve(mcp::Gild { core: core.clone(), app: Some(app.handle().clone()) });
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
            create_agent,
            channel_access,
            set_access,
            activity,
            mcp_status,
            export_workspace,
        ])
        .run(tauri::generate_context!())
        .expect("error while running gild desktop");
}
