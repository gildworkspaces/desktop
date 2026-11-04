mod store;

use std::sync::Mutex;

use serde::Serialize;
use store::{Channel, Message, Store, Workspace, DEMO_NAME};
use tauri::{Manager, State};

/// The one store, opened when the app starts.
struct Core(Mutex<Store>);

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

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let store = Store::open(&dir.join("gild.db"))?;
            store.seed_if_empty()?;
            app.manage(Core(Mutex::new(store)));
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running gild desktop");
}
