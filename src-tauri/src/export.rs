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
//!   channels/<name>/docs/*.md      the channel's docs, as they are now
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

/// Copy a folder. Symbolic links are skipped, never followed: an export
/// someone hands you can't pull other files from your computer in.
pub(crate) fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| err("copying history", e))?;
    for entry in fs::read_dir(from).map_err(|e| err("copying history", e))? {
        let entry = entry.map_err(|e| err("copying history", e))?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type().map_err(|e| err("copying history", e))?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
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
    for m in threaded(messages) {
        let who = if m.kind == "agent" { format!("{} (agent)", m.author) } else { m.author.clone() };
        if m.thread.is_some() {
            // A reply: quoted under the message its thread hangs off.
            let body: String = m.body.lines().map(|l| format!("> {l}\n")).collect();
            out.push_str(&format!("> ↳ **{who}** · {} {}\n>\n{body}\n", day(m.created_at), time(m.created_at)));
            continue;
        }
        let d = day(m.created_at);
        if d != current {
            out.push_str(&format!("\n## {d}\n\n"));
            current = d;
        }
        out.push_str(&format!("**{who}** · {}\n\n{}\n\n", time(m.created_at), m.body));
    }
    out
}

/// Messages in reading order: each one followed by its thread's replies.
pub fn threaded(messages: &[Message]) -> Vec<&Message> {
    let mut out = Vec::new();
    for m in messages.iter().filter(|m| m.thread.is_none()) {
        out.push(m);
        out.extend(messages.iter().filter(|r| r.thread.as_ref() == Some(&m.id)));
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
        // Every message, replies too, each reply naming its thread.
        let messages = store.all_messages(&c.id)?;
        let data = store.channel_data(&c.id)?;
        write(&dir.join("chat.md"), chat_md(&c, &messages))?;
        let jsonl: String = messages.iter().map(|m| serde_json::to_string(m).unwrap_or_default() + "\n").collect();
        write(&dir.join("chat.jsonl"), jsonl)?;
        let table = data.view.as_ref().map(|v| json!({ "columns": v.fields, "statuses": v.statuses, "records": data.records }));
        if let (Some(csv), Some(t)) = (table_csv(&data), &table) {
            write(&dir.join("table.csv"), csv)?;
            write(&dir.join("table.json"), serde_json::to_vec_pretty(t).map_err(|e| err("writing table.json", e))?)?;
        }
        let docs = store.docs(&c.id)?;
        for d in &docs {
            // A doc's path is docs/<name>.md, made by channel_name(): already a
            // safe file name, but keep it inside the export regardless.
            let name = safe(d.path.trim_start_matches("docs/").trim_end_matches(".md"));
            write(&dir.join("docs").join(format!("{name}.md")), &d.body)?;
        }
        let repo = records::repo_path(store.data_dir(), &c.id);
        if repo.exists() {
            copy_dir(&repo, &dir.join("history.git"))?;
        }
        let docs: Vec<_> = docs.iter().map(|d| json!({ "path": d.path, "title": d.title, "text": d.body })).collect();
        channels.push(json!({ "name": c.name, "topic": c.topic, "messages": threaded(&messages), "table": table, "docs": docs }));
    }
    let manifest = json!({
        "workspace": ws.name,
        "exported_at": crate::store::now(),
        "gild_version": env!("CARGO_PKG_VERSION"),
        "channels": channels,
    });
    let js = format!("{MANIFEST_START}{};\n", serde_json::to_string(&manifest).map_err(|e| err("writing the viewer's data", e))?);
    write(&root.join("data").join("workspace.js"), js)?;
    write(&root.join("export-viewer.html"), VIEWER)?;
    write(
        &root.join("README.md"),
        format!(
            "# {}\n\nExported from gild {} on {}.\n\nOpen `export-viewer.html` in a browser to read everything, offline.\n\nEach channel is a folder in `channels/`: `chat.md` and `chat.jsonl` hold the conversation, `table.csv` and `table.json` hold its table (if it has one), `docs/` holds its docs as markdown, and `history.git` is the channel's full history, which `git clone` opens.\n",
            ws.name,
            env!("CARGO_PKG_VERSION"),
            day(crate::store::now())
        ),
    )?;
    Ok(root)
}

const MANIFEST_START: &str = "window.GILD_EXPORT = ";

/// Make an export folder a workspace again. The open files are the source:
/// each channel's chat from `chat.jsonl` and its history from `history.git`
/// (its table, docs, access and open proposals are rebuilt from that); the
/// workspace and channel names and topics come from `data/workspace.js`.
pub fn import_workspace(store: &Store, folder: &Path) -> Result<crate::store::Workspace, String> {
    let not_export = || "This folder isn't a gild export: it has no data/workspace.js from gild. Pick the folder Export workspace made.".to_string();
    let js = fs::read_to_string(folder.join("data").join("workspace.js")).map_err(|_| not_export())?;
    let json = js.trim().strip_prefix(MANIFEST_START).and_then(|s| s.strip_suffix(';')).ok_or_else(not_export)?;
    let manifest: serde_json::Value = serde_json::from_str(json).map_err(|e| err("reading data/workspace.js", e))?;
    let name = manifest["workspace"].as_str().ok_or_else(not_export)?;
    let mut channels = Vec::new();
    for c in manifest["channels"].as_array().ok_or_else(not_export)? {
        let cname = c["name"].as_str().ok_or_else(not_export)?.to_string();
        let dir = folder.join("channels").join(safe(&cname));
        let messages: Vec<Message> = match fs::read_to_string(dir.join("chat.jsonl")) {
            Ok(lines) => lines
                .lines()
                .filter(|l| !l.trim().is_empty())
                .enumerate()
                .map(|(i, l)| serde_json::from_str(l).map_err(|e| err(&format!("reading #{cname}'s chat.jsonl, line {}", i + 1), e)))
                .collect::<Result<_, _>>()?,
            Err(_) => serde_json::from_value(c["messages"].clone()).map_err(|e| err(&format!("reading #{cname}'s messages"), e))?,
        };
        let history = dir.join("history.git");
        channels.push(crate::store::ImportChannel {
            name: cname,
            topic: c["topic"].as_str().unwrap_or("").to_string(),
            messages,
            history: history.is_dir().then_some(history),
        });
    }
    store.import_workspace(name, channels)
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
        let sop = store.create_doc(&intake.id, DEMO_NAME, "Intake SOP").unwrap();
        store.save_doc(&intake.id, DEMO_NAME, &sop.path, "# Intake SOP\n\nForms go in the blue folder.\n").unwrap();

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
        assert_eq!(head.message_raw_sloppy().to_string().trim(), "Edit Intake SOP");
        assert_eq!(fs::read_to_string(ch.join("docs/intake-sop.md")).unwrap(), "# Intake SOP\n\nForms go in the blue folder.\n");
        // The viewer and its data sit beside the files, with no network.
        let viewer = fs::read_to_string(out.join("export-viewer.html")).unwrap();
        assert!(viewer.contains("default-src 'none'") && !viewer.contains("innerHTML"));
        let js = fs::read_to_string(out.join("data/workspace.js")).unwrap();
        assert!(js.starts_with("window.GILD_EXPORT = ") && js.contains("Jordan Lee") && js.contains("blue folder"));
        // A second export with the same time doesn't overwrite the first.
        let again = export_workspace(&store, &ws.id, &base.join("exports"), "2026-11-04 1200").unwrap();
        assert_eq!(again.file_name().unwrap(), "Front Desk 2026-11-04 1200 (2)");
        assert!(out.join("export-viewer.html").exists());
        drop(store);
        fs::remove_dir_all(base).unwrap();
    }

    /// Every message line without the ids, which a new home gives anew.
    /// The chat with ids left out, and each reply's thread given as the
    /// position of the message it hangs off.
    fn chat_without_ids(path: &Path) -> Vec<serde_json::Value> {
        let lines: Vec<serde_json::Value> = fs::read_to_string(path).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let ids: Vec<String> = lines.iter().map(|v| v["id"].as_str().unwrap().to_string()).collect();
        lines
            .into_iter()
            .map(|mut v| {
                if let Some(t) = v["thread"].as_str() {
                    v["thread"] = serde_json::json!(ids.iter().position(|i| i == t).expect("a reply's thread is in the chat"));
                }
                v.as_object_mut().unwrap().retain(|k, _| k != "id" && k != "channel_id");
                v
            })
            .collect()
    }

    fn refs(repo: &Path) -> Vec<(String, String)> {
        let repo = gix::open(repo).unwrap();
        let mut out: Vec<(String, String)> = repo
            .references()
            .unwrap()
            .all()
            .unwrap()
            .map(|r| {
                let mut r = r.unwrap();
                (r.name().as_bstr().to_string(), r.peel_to_id().unwrap().to_string())
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn export_then_import_then_export_gives_the_same_workspace() {
        let base = std::env::temp_dir().join(format!("gild-roundtrip-{}-{}", std::process::id(), line!()));
        let (a, b) = (base.join("a"), base.join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let store = Store::open(&a).unwrap();
        let ws = store.create_workspace("Front Desk").unwrap();
        let general = store.channels(&ws.id).unwrap()[0].clone();
        let intake = store.create_channel(&ws.id, "student-intake", "Students sent by partner schools").unwrap();
        let empty = store.create_channel(&ws.id, "quiet", "").unwrap();
        store.post(&general.id, DEMO_NAME, "person", "Morning, all. \"Quotes\", commas, and émoji 🎓").unwrap();
        store.set_view(&intake.id, DEMO_NAME, &["Sent by".into()], &["New".into(), "Enrolled".into()]).unwrap();
        let jordan = store.add_record(&intake.id, DEMO_NAME, "Jordan Lee", "New", [("Sent by".to_string(), "Northside".to_string())].into()).unwrap();
        store.update_record(&intake.id, DEMO_NAME, &jordan.id, "Jordan Lee", "Enrolled", [("Sent by".to_string(), "Northside".to_string())].into()).unwrap();
        let (agent, _) = store.create_agent("intake").unwrap();
        store.set_access(&intake.id, DEMO_NAME, &agent.id, Some(crate::records::Access::Write)).unwrap();
        let sop = store.create_doc(&intake.id, DEMO_NAME, "Intake SOP").unwrap();
        store.save_doc(&intake.id, DEMO_NAME, &sop.path, "# Intake SOP\n\nForms go in the blue folder.\n").unwrap();
        store.propose_doc(&intake.id, "intake", Some(&sop.path), None, "# Intake SOP\n\nForms go in the red folder.\n", "Red now").unwrap();
        let enrolled = store.post(&intake.id, "intake", "agent", "Jordan is enrolled.").unwrap();
        store.post_in(&intake.id, Some(&enrolled.id), DEMO_NAME, "person", "Which program?").unwrap();
        let first = export_workspace(&store, &ws.id, &base.join("out1"), "one").unwrap();
        drop(store);

        // A new computer: import, and export again.
        let other = Store::open(&b).unwrap();
        let back = import_workspace(&other, &first).unwrap();
        assert_eq!((back.name.as_str(), back.channels, back.messages), ("Front Desk", 3, 4));
        let names: Vec<String> = other.channels(&back.id).unwrap().into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["general", "student-intake", "quiet"], "in the export's order");
        let intake2 = other.channels(&back.id).unwrap().into_iter().find(|c| c.name == "student-intake").unwrap();
        let top = other.messages(&intake2.id).unwrap();
        let enrolled2 = top.iter().find(|m| m.body == "Jordan is enrolled.").unwrap();
        assert_eq!(enrolled2.replies, 1, "the thread came back, not as a channel message");
        assert_eq!(other.thread(&intake2.id, &enrolled2.id).unwrap()[1].body, "Which program?");
        assert_eq!(other.proposals(&intake2.id).unwrap()[0].summary, "Red now", "open proposals come back");
        assert!(other.agents().unwrap().is_empty() && other.pending_mentions(&agent.id).unwrap().is_empty(), "importing wakes no one");
        let second = export_workspace(&other, &back.id, &base.join("out2"), "two").unwrap();

        for c in ["general", "student-intake", "quiet"] {
            let (x, y) = (first.join("channels").join(c), second.join("channels").join(c));
            assert_eq!(chat_without_ids(&x.join("chat.jsonl")), chat_without_ids(&y.join("chat.jsonl")), "#{c} chat");
            assert_eq!(fs::read_to_string(x.join("chat.md")).unwrap(), fs::read_to_string(y.join("chat.md")).unwrap(), "#{c} chat.md");
            for f in ["table.csv", "table.json", "docs/intake-sop.md"] {
                assert_eq!(fs::read(x.join(f)).ok(), fs::read(y.join(f)).ok(), "#{c} {f}");
            }
            assert_eq!(x.join("history.git").exists(), y.join("history.git").exists(), "#{c} history");
            if x.join("history.git").exists() {
                assert_eq!(refs(&x.join("history.git")), refs(&y.join("history.git")), "#{c}: the same commits, branches and all");
            }
        }
        assert!(first.join("channels/student-intake/docs/intake-sop.md").exists() && !empty.id.is_empty());
        drop(other);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn import_refuses_what_isn_t_an_export_and_never_follows_links() {
        let base = std::env::temp_dir().join(format!("gild-import-{}-{}", std::process::id(), line!()));
        fs::create_dir_all(base.join("data")).unwrap();
        let store = Store::open(&base.join("data")).unwrap();
        fs::create_dir_all(base.join("random")).unwrap();
        assert!(import_workspace(&store, &base.join("random")).unwrap_err().contains("isn't a gild export"));

        let ws = store.create_workspace("W").unwrap();
        let general = store.channels(&ws.id).unwrap()[0].clone();
        store.create_doc(&general.id, DEMO_NAME, "Notes").unwrap();
        let out = export_workspace(&store, &ws.id, &base.join("out"), "x").unwrap();
        // Someone slips a link to a private file into the history.
        let secret = base.join("secret.txt");
        fs::write(&secret, "private").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, out.join("channels/general/history.git/stolen")).unwrap();
        let back = import_workspace(&store, &out).unwrap();
        let id = store.channels(&back.id).unwrap()[0].id.clone();
        assert!(!crate::records::repo_path(store.data_dir(), &id).join("stolen").exists(), "the link wasn't followed");
        assert_eq!(store.docs(&id).unwrap()[0].title, "Notes");

        // A hand-edited export with a bad channel name is refused before anything is written.
        let js = fs::read_to_string(out.join("data/workspace.js")).unwrap().replace("\"name\":\"general\"", "\"name\":\"../general\"");
        fs::write(out.join("data/workspace.js"), js).unwrap();
        let before = store.workspaces().unwrap().len();
        assert!(import_workspace(&store, &out).unwrap_err().contains("isn't a channel name"));
        assert_eq!(store.workspaces().unwrap().len(), before);
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
