//! Add-ons: how gild grows past what ships in the box.
//!
//! An add-on is a package with a manifest (`gild-addon.json`) saying what it
//! contributes and what it may touch. In this first version it contributes
//! file types: a file extension, an editor, and the editor's *operations*,
//! typed and described in the manifest. Operations are the contract: the
//! window's own edits go through them, every one becomes an agent tool
//! (`<file type>_<operation>`), and each is a commit and an Activity entry,
//! so a file type works with agents, history and the shared view without its
//! author doing anything more.
//!
//! gild's own file types are add-ons too, built against the same interface.
//! Their logic runs in the core for now; third-party logic will run as a
//! WebAssembly component behind this same `Editor` trait, reaching the
//! network only through gild and only to the hosts its manifest names.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub description: String,
    pub contributes: Contributes,
    pub permissions: Permissions,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Contributes {
    #[serde(default)]
    pub file_types: Vec<FileType>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FileType {
    pub id: String,
    pub name: String,
    pub extensions: Vec<String>,
    pub operations: Vec<Operation>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Operation {
    pub name: String,
    pub description: String,
    /// A JSON Schema object for the operation's arguments.
    pub params: Value,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Permissions {
    /// Hosts it may reach, through gild.
    #[serde(default)]
    pub network: Vec<String>,
    /// Secrets the person enters in Settings; gild uses them for it.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// The files it may touch in a channel, as globs.
    #[serde(default)]
    pub files: Vec<String>,
}

/// What a file type's editor does: apply one operation to a file's text.
pub trait Editor: Send + Sync {
    fn apply(&self, text: &str, op: &str, args: &Value) -> Result<String, String>;
}

struct Addon {
    manifest: Manifest,
    /// One editor per file type, by the file type's id.
    editors: Vec<(String, Box<dyn Editor>)>,
}

pub struct Registry {
    addons: Vec<Addon>,
}

/// A file type's operation as an agent tool.
pub struct ToolOp<'a> {
    pub tool: String,
    pub file_type: &'a FileType,
    pub op: &'a Operation,
}

impl Registry {
    /// The add-ons that ship with gild.
    pub fn built_in() -> Registry {
        let markdown: Manifest = serde_json::from_str(include_str!("../addons/markdown/gild-addon.json")).expect("the markdown add-on's manifest is valid");
        Registry { addons: vec![Addon { manifest: markdown, editors: vec![("markdown".into(), Box::new(Markdown))] }] }
    }

    pub fn manifests(&self) -> Vec<&Manifest> {
        self.addons.iter().map(|a| &a.manifest).collect()
    }

    /// The file type a path is, by its extension.
    pub fn file_type(&self, path: &str) -> Option<&FileType> {
        let lower = path.to_lowercase();
        self.addons.iter().flat_map(|a| &a.manifest.contributes.file_types).find(|t| t.extensions.iter().any(|e| lower.ends_with(&e.to_lowercase())))
    }

    /// Every operation of every file type, as agent tools.
    pub fn tools(&self) -> Vec<ToolOp<'_>> {
        self.addons
            .iter()
            .flat_map(|a| &a.manifest.contributes.file_types)
            .flat_map(|t| t.operations.iter().map(move |op| ToolOp { tool: format!("{}_{}", t.id, op.name), file_type: t, op }))
            .collect()
    }

    pub fn tool(&self, name: &str) -> Option<ToolOp<'_>> {
        self.tools().into_iter().find(|t| t.tool == name)
    }

    /// Apply an operation to a file's text: the file type's editor, after
    /// checking the arguments against the operation's declared params.
    pub fn apply(&self, path: &str, text: &str, op: &str, args: &Value) -> Result<String, String> {
        let file_type = self.file_type(path).ok_or_else(|| format!("No add-on edits {path}. It can be read, not edited."))?;
        let operation = file_type
            .operations
            .iter()
            .find(|o| o.name == op)
            .ok_or_else(|| format!("{} files have no operation “{op}”. They have: {}.", file_type.name, file_type.operations.iter().map(|o| o.name.as_str()).collect::<Vec<_>>().join(", ")))?;
        for field in operation.params["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if args.get(field).and_then(Value::as_str).is_none() {
                return Err(format!("{op} needs “{field}” (text)."));
            }
        }
        let editor = self
            .addons
            .iter()
            .flat_map(|a| &a.editors)
            .find(|(id, _)| id == &file_type.id)
            .map(|(_, e)| e)
            .ok_or_else(|| format!("The {} editor isn't available.", file_type.name))?;
        editor.apply(text, op, args)
    }
}

/// gild's markdown editor.
struct Markdown;

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&level).then(|| line[level..].strip_prefix(' ')).flatten().map(|t| (level, t.trim()))
}

fn arg<'a>(args: &'a Value, name: &str) -> &'a str {
    args.get(name).and_then(Value::as_str).unwrap_or("")
}

impl Markdown {
    /// The section under a heading: (heading line, first line after the section).
    fn section(lines: &[&str], wanted: &str) -> Result<(usize, usize), String> {
        let want = wanted.trim().trim_start_matches('#').trim().to_lowercase();
        let (start, level) = lines
            .iter()
            .enumerate()
            .find_map(|(i, l)| heading(l).filter(|(_, t)| t.to_lowercase() == want).map(|(lv, _)| (i, lv)))
            .ok_or_else(|| {
                let all: Vec<&str> = lines.iter().filter_map(|l| heading(l).map(|(_, t)| t)).collect();
                format!("No heading “{wanted}” in this file. Its headings: {}.", if all.is_empty() { "none".into() } else { all.join(", ") })
            })?;
        let end = lines.iter().enumerate().skip(start + 1).find(|(_, l)| heading(l).is_some_and(|(lv, _)| lv <= level)).map(|(i, _)| i).unwrap_or(lines.len());
        Ok((start, end))
    }

    fn join(before: &[&str], middle: &str, after: &[&str]) -> String {
        let mut out = before.join("\n");
        let middle = middle.trim_matches('\n');
        if !middle.is_empty() {
            out.push_str("\n\n");
            out.push_str(middle);
        }
        let rest = after.join("\n");
        let rest = rest.trim_start_matches('\n');
        if !rest.is_empty() {
            out.push_str("\n\n");
            out.push_str(rest);
        }
        out.push('\n');
        out
    }
}

impl Editor for Markdown {
    fn apply(&self, text: &str, op: &str, args: &Value) -> Result<String, String> {
        let lines: Vec<&str> = text.lines().collect();
        match op {
            "replace_section" => {
                let (start, end) = Self::section(&lines, arg(args, "heading"))?;
                Ok(Self::join(&lines[..=start], arg(args, "text"), &lines[end..]))
            }
            "insert_after" => {
                let (start, end) = Self::section(&lines, arg(args, "heading"))?;
                let body = lines[start + 1..end].join("\n");
                let body = body.trim_matches('\n');
                let added = arg(args, "text").trim_matches('\n');
                let middle = if body.is_empty() { added.to_string() } else { format!("{body}\n\n{added}") };
                Ok(Self::join(&lines[..=start], &middle, &lines[end..]))
            }
            "replace_text" => {
                let find = arg(args, "find");
                match text.matches(find).count() {
                    _ if find.is_empty() => Err("“find” is empty.".into()),
                    1 => Ok(text.replacen(find, arg(args, "replace"), 1)),
                    0 => Err("That text isn't in the file.".into()),
                    n => Err(format!("That text appears {n} times; give more of it so it's found once.")),
                }
            }
            "append" => {
                let base = text.trim_end_matches('\n');
                let added = arg(args, "text").trim_matches('\n');
                Ok(if base.is_empty() { format!("{added}\n") } else { format!("{base}\n\n{added}\n") })
            }
            "set_text" => Ok(arg(args, "text").to_string()),
            other => Err(format!("Markdown has no operation “{other}”.")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SOP: &str = "# Intake SOP\n\n## Step 1: Greet\n\nSay hello.\n\n## Step 2: Forms\n\nSigned forms go in the blue folder.\n\n### Note\n\nAsk twice.\n\n## Step 3: Done\n\nWave.\n";

    #[test]
    fn the_markdown_add_on_is_a_manifest_and_its_operations_are_tools() {
        let r = Registry::built_in();
        assert_eq!(r.manifests().iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["gild.markdown"]);
        assert_eq!(r.file_type("docs/Intake SOP.MD").map(|t| t.id.as_str()), Some("markdown"));
        assert!(r.file_type("data/table.csv").is_none());
        let tools: Vec<String> = r.tools().into_iter().map(|t| t.tool).collect();
        assert_eq!(tools, ["markdown_replace_section", "markdown_insert_after", "markdown_replace_text", "markdown_append", "markdown_set_text"]);
        assert!(r.tool("markdown_replace_section").is_some_and(|t| t.op.params["required"] == json!(["heading", "text"])));
    }

    #[test]
    fn markdown_operations() {
        let r = Registry::built_in();
        let run = |op: &str, args: Value| r.apply("docs/sop.md", SOP, op, &args);
        // A section ends at the next heading of its level or higher: its sub-section goes with it.
        assert_eq!(
            run("replace_section", json!({ "heading": "Step 2: Forms", "text": "They go in the red folder." })).unwrap(),
            "# Intake SOP\n\n## Step 1: Greet\n\nSay hello.\n\n## Step 2: Forms\n\nThey go in the red folder.\n\n## Step 3: Done\n\nWave.\n"
        );
        assert_eq!(
            run("insert_after", json!({ "heading": "step 1: greet", "text": "Offer water." })).unwrap(),
            "# Intake SOP\n\n## Step 1: Greet\n\nSay hello.\n\nOffer water.\n\n## Step 2: Forms\n\nSigned forms go in the blue folder.\n\n### Note\n\nAsk twice.\n\n## Step 3: Done\n\nWave.\n"
        );
        assert_eq!(run("replace_text", json!({ "find": "blue", "replace": "red" })).unwrap(), SOP.replace("blue", "red"));
        assert!(run("replace_text", json!({ "find": "folder", "replace": "box" })).is_ok());
        assert!(run("replace_text", json!({ "find": "Step", "replace": "x" })).unwrap_err().contains("appears 3 times"));
        assert!(run("replace_text", json!({ "find": "nope", "replace": "x" })).unwrap_err().contains("isn't in the file"));
        assert!(run("append", json!({ "text": "Questions? Ask #help." })).unwrap().ends_with("Wave.\n\nQuestions? Ask #help.\n"));
        assert_eq!(run("set_text", json!({ "text": "# New\n" })).unwrap(), "# New\n");
        // Wrong heading, op or arguments: said plainly, nothing changed.
        let e = run("replace_section", json!({ "heading": "Step 9", "text": "x" })).unwrap_err();
        assert!(e.contains("Its headings: Intake SOP, Step 1: Greet, Step 2: Forms, Note, Step 3: Done"), "{e}");
        assert!(run("delete_everything", json!({})).unwrap_err().contains("They have: replace_section"));
        assert!(run("replace_section", json!({ "text": "x" })).unwrap_err().contains("needs “heading”"));
        assert!(r.apply("data/table.csv", "a,b", "set_text", &json!({ "text": "" })).unwrap_err().contains("can be read, not edited"));
    }
}
