//! Export a workspace as a folder of open files: what you'd need to leave
//! gild with everything, and to read it without gild.
//!
//!   README.md                      what's here
//!   export-viewer.html             open it to browse everything, offline
//!   data/workspace.js              what the viewer reads
//!   channels/<name>/chat.md        the conversation, by day
//!   channels/<name>/chat.jsonl     the same, one JSON message per line
//!   channels/<name>/table.csv      the channel's table, if it has one
//!   channels/<name>/table.json
//!   channels/<name>/history.git    the channel's full history: `git clone` it
//!
//! Every format is written from the same rows (one read per channel), so the
//! files and the viewer can't disagree.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::records;
use crate::store::{Channel, ChannelData, Message, Store};

const VIEWER: &str = include_str!("../export-viewer.html");

fn err(context: &str, e: impl std::fmt::Display) -> String {
    format!("{context}: {e}")
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| err("creating a folder", e))?;
    }
    fs::write(path, bytes).map_err(|e| err(&format!("writing {}", path.display()), e))
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| err("copying history", e))?;
    for entry in fs::read_dir(from).map_err(|e| err("copying history", e))? {
        let entry = entry.map_err(|e| err("copying history", e))?;
        let target = to.join(entry.file_name());
        if entry.file_type().map_err(|e| err("copying history", e))?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target).map_err(|e| err("copying history", e))?;
        }
    }
    Ok(())
}

/// One CSV field: quoted when it has a comma, a quote or a line break.
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub fn table_csv(data: &ChannelData) -> Option<String> {
    let view = data.view.as_ref()?;
    let mut out = String::new();
    let header: Vec<String> = std::iter::once("Title").chain(view.fields.iter().map(String::as_str)).chain(["Status"]).map(csv_field).collect();
    out.push_str(&header.join(","));
    out.push('\n');
    for r in &data.records {
        let row: Vec<String> = std::iter::once(r.title.as_str())
            .chain(view.fields.iter().map(|f| r.fields.get(f).map(String::as_str).unwrap_or("")))
            .chain([r.status.as_str()])
            .map(csv_field)
            .collect();
        out.push_str(&row.join(","));
        out.push('\n');
    }
    Some(out)
}

/// The UTC date of a Unix time in milliseconds, as YYYY-MM-DD.
pub fn date(ms: i64) -> String {
    day(ms)
}

fn day(ms: i64) -> String {
    // A civil date from a Unix time (UTC), without a date library.
    let days = ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn time(ms: i64) -> String {
    let s = ms.div_euclid(1000).rem_euclid(86_400);
    format!("{:02}:{:02} UTC", s / 3600, s / 60 % 60)
}

pub fn chat_md(channel: &Channel, messages: &[Message]) -> String {
    let mut out = format!("# #{}\n", channel.name);
    if !channel.topic.is_empty() {
        out.push_str(&format!("\n{}\n", channel.topic));
    }
    let mut current = String::new();
    for m in messages {
        let d = day(m.created_at);
        if d != current {
            out.push_str(&format!("\n## {d}\n\n"));
            current = d;
        }
        let who = if m.kind == "agent" { format!("{} (agent)", m.author) } else { m.author.clone() };
        out.push_str(&format!("**{who}** · {}\n\n{}\n\n", time(m.created_at), m.body));
    }
    out
}

/// A folder name from anything a person typed: no separators or dots at the
/// start, so it stays inside the export.
fn safe(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '-' }).collect();
    let s = s.trim().trim_start_matches(['.', '-']).to_string();
    if s.is_empty() { "untitled".into() } else { s }
}

/// Write the workspace into a new folder inside `parent` and return it.
pub fn export_workspace(store: &Store, workspace_id: &str, parent: &Path, stamp: &str) -> Result<PathBuf, String> {
    let ws = store.workspaces()?.into_iter().find(|w| w.id == workspace_id).ok_or("That workspace no longer exists.")?;
    // Never overwrite an earlier export: a second one in the same second gets
    // " (2)", and so on.
    let base = format!("{} {stamp}", safe(&ws.name));
    let mut root = parent.join(&base);
    let mut n = 2;
    while root.exists() {
        root = parent.join(format!("{base} ({n})"));
        n += 1;
    }
    let mut channels = Vec::new();
    for c in store.channels(&ws.id)? {
        let dir = root.join("channels").join(safe(&c.name));
        let messages = store.messages(&c.id)?;
        let data = store.channel_data(&c.id)?;
        write(&dir.join("chat.md"), chat_md(&c, &messages))?;
        let jsonl: String = messages.iter().map(|m| serde_json::to_string(m).unwrap_or_default() + "\n").collect();
        write(&dir.join("chat.jsonl"), jsonl)?;
        let table = data.view.as_ref().map(|v| json!({ "columns": v.fields, "statuses": v.statuses, "records": data.records }));
        if let (Some(csv), Some(t)) = (table_csv(&data), &table) {
            write(&dir.join("table.csv"), csv)?;
            write(&dir.join("table.json"), serde_json::to_vec_pretty(t).map_err(|e| err("writing table.json", e))?)?;
        }
        let repo = records::repo_path(store.data_dir(), &c.id);
        if repo.exists() {
            copy_dir(&repo, &dir.join("history.git"))?;
        }
        channels.push(json!({ "name": c.name, "topic": c.topic, "messages": messages, "table": table }));
    }
    let manifest = json!({
        "workspace": ws.name,
        "exported_at": crate::store::now(),
        "gild_version": env!("CARGO_PKG_VERSION"),
        "channels": channels,
    });
    let js = format!("window.GILD_EXPORT = {};\n", serde_json::to_string(&manifest).map_err(|e| err("writing the viewer's data", e))?);
    write(&root.join("data").join("workspace.js"), js)?;
    write(&root.join("export-viewer.html"), VIEWER)?;
    write(
        &root.join("README.md"),
        format!(
            "# {}\n\nExported from gild {} on {}.\n\nOpen `export-viewer.html` in a browser to read everything, offline.\n\nEach channel is a folder in `channels/`: `chat.md` and `chat.jsonl` hold the conversation, `table.csv` and `table.json` hold its table (if it has one), and `history.git` is the channel's full history, which `git clone` opens.\n",
            ws.name,
            env!("CARGO_PKG_VERSION"),
            day(crate::store::now())
        ),
    )?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::DEMO_NAME;
    use std::collections::BTreeMap;

    #[test]
    fn an_export_holds_everything_in_open_files() {
        let base = std::env::temp_dir().join(format!("gild-export-{}-{}", std::process::id(), line!()));
        let data = base.join("data");
        fs::create_dir_all(&data).unwrap();
        let store = Store::open(&data).unwrap();
        let ws = store.create_workspace("Front Desk").unwrap();
        let intake = store.create_channel(&ws.id, "student-intake", "Students sent by partner schools").unwrap();
        store.set_view(&intake.id, DEMO_NAME, &["Sent by".into(), "Notes".into()], &["New".into(), "Enrolled".into()]).unwrap();
        let fields: BTreeMap<String, String> = [("Sent by".to_string(), "Northside, East".to_string()), ("Notes".to_string(), "said \"soon\"".to_string())].into();
        store.add_record(&intake.id, DEMO_NAME, "Jordan Lee", "New", fields).unwrap();
        store.post(&intake.id, DEMO_NAME, "person", "<script>alert(1)</script> three more students").unwrap();

        let out = export_workspace(&store, &ws.id, &base.join("exports"), "2026-11-04 1200").unwrap();
        let ch = out.join("channels/student-intake");
        let csv = fs::read_to_string(ch.join("table.csv")).unwrap();
        assert_eq!(csv, "Title,Sent by,Notes,Status\nJordan Lee,\"Northside, East\",\"said \"\"soon\"\"\",New\n");
        let chat = fs::read_to_string(ch.join("chat.md")).unwrap();
        assert!(chat.contains("**Demo**") && chat.contains("three more students"), "{chat}");
        let lines: Vec<serde_json::Value> = fs::read_to_string(ch.join("chat.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 1);
        // The history is a real git repository with the record's commit in it.
        let repo = gix::open(ch.join("history.git")).unwrap();
        let head = repo.head_commit().unwrap();
        assert_eq!(head.message_raw_sloppy().to_string().trim(), "Add Jordan Lee");
        // The viewer and its data sit beside the files, with no network.
        let viewer = fs::read_to_string(out.join("export-viewer.html")).unwrap();
        assert!(viewer.contains("default-src 'none'") && !viewer.contains("innerHTML"));
        let js = fs::read_to_string(out.join("data/workspace.js")).unwrap();
        assert!(js.starts_with("window.GILD_EXPORT = ") && js.contains("Jordan Lee"));
        // A second export with the same time doesn't overwrite the first.
        let again = export_workspace(&store, &ws.id, &base.join("exports"), "2026-11-04 1200").unwrap();
        assert_eq!(again.file_name().unwrap(), "Front Desk 2026-11-04 1200 (2)");
        assert!(out.join("export-viewer.html").exists());
        drop(store);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn folder_names_stay_inside_the_export() {
        assert_eq!(safe("../../etc"), "etc");
        assert!(!safe("../x").contains('/'));
        assert_eq!(safe(""), "untitled");
        assert_eq!(safe("Front Desk"), "Front Desk");
    }

    #[test]
    fn days_and_times() {
        assert_eq!(day(0), "1970-01-01");
        assert_eq!(day(1_793_750_400_000), "2026-11-04");
        assert_eq!(time(1_793_750_400_000 + 3_723_000), "01:02 UTC");
    }
}
