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
//! An add-on's logic is a WebAssembly module (`WasmEditor`): it imports
//! nothing, so it can't reach files, the network or the clock; each call
//! has a memory limit and an instruction budget. gild's own file types are
//! add-ons built the same way (`addons/<name>`, compiled by build.rs), so the
//! app runs exactly what a third party would ship.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wasmtime::{Config, Engine, Instance, Module, Store, StoreLimits, StoreLimitsBuilder};

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

impl Addon {
    /// An add-on from its manifest and its module: the module edits every
    /// file type the manifest names.
    fn load(manifest: &str, module: &[u8]) -> Result<Addon, String> {
        let manifest: Manifest = serde_json::from_str(manifest).map_err(|e| format!("its gild-addon.json: {e}"))?;
        let editors = manifest
            .contributes
            .file_types
            .iter()
            .map(|t| WasmEditor::new(&manifest.name, module).map(|e| (t.id.clone(), Box::new(e) as Box<dyn Editor>)))
            .collect::<Result<_, _>>()?;
        Ok(Addon { manifest, editors })
    }
}

/// How much an add-on call may use: memory, and instructions (wasmtime's
/// fuel, about one per instruction).
pub const ADDON_MEMORY: usize = 64 << 20;
pub const ADDON_FUEL: u64 = 2_000_000_000;

/// An editor that is a WebAssembly module, run in a sandbox: it may import
/// nothing, and every call starts fresh, with ADDON_MEMORY and ADDON_FUEL.
pub struct WasmEditor {
    name: String,
    engine: Engine,
    module: Module,
}

impl WasmEditor {
    pub fn new(name: &str, bytes: &[u8]) -> Result<WasmEditor, String> {
        let mut config = Config::new();
        config.consume_fuel(true);
        let engine = Engine::new(&config).map_err(|e| format!("starting the add-on sandbox: {e}"))?;
        let module = Module::new(&engine, bytes).map_err(|e| format!("{name} isn't a WebAssembly module gild can run: {e}"))?;
        if let Some(i) = module.imports().next() {
            return Err(format!("{name} asks for {}.{}; add-ons may import nothing (they reach the world only through gild).", i.module(), i.name()));
        }
        for export in ["memory", "gild_alloc", "gild_apply"] {
            if module.get_export(export).is_none() {
                return Err(format!("{name} has no `{export}`, which every add-on editor exports."));
            }
        }
        Ok(WasmEditor { name: name.to_string(), engine, module })
    }

    fn call(&self, request: &[u8]) -> Result<Vec<u8>, String> {
        let limits = StoreLimitsBuilder::new().memory_size(ADDON_MEMORY).instances(1).build();
        let mut store: Store<StoreLimits> = Store::new(&self.engine, limits);
        store.limiter(|l| l);
        store.set_fuel(ADDON_FUEL).map_err(|e| e.to_string())?;
        let failed = |e: wasmtime::Error| {
            if matches!(e.downcast_ref::<wasmtime::Trap>(), Some(wasmtime::Trap::OutOfFuel)) {
                format!("The {} add-on took too long, so gild stopped it.", self.name)
            } else {
                format!("The {} add-on failed: {e}", self.name)
            }
        };
        let instance = Instance::new(&mut store, &self.module, &[]).map_err(failed)?;
        let memory = instance.get_memory(&mut store, "memory").ok_or("no memory")?;
        let alloc = instance.get_typed_func::<i32, i32>(&mut store, "gild_alloc").map_err(failed)?;
        let apply = instance.get_typed_func::<(i32, i32), i64>(&mut store, "gild_apply").map_err(failed)?;
        let len = i32::try_from(request.len()).map_err(|_| "too big for an add-on".to_string())?;
        let ptr = alloc.call(&mut store, len).map_err(failed)?;
        memory.write(&mut store, ptr as u32 as usize, request).map_err(|e| format!("The {} add-on's memory: {e}", self.name))?;
        let answer = apply.call(&mut store, (ptr, len)).map_err(failed)? as u64;
        let (at, n) = ((answer >> 32) as usize, (answer & 0xffff_ffff) as usize);
        let mut out = vec![0; n];
        memory.read(&store, at, &mut out).map_err(|e| format!("The {} add-on's answer: {e}", self.name))?;
        Ok(out)
    }
}

impl Editor for WasmEditor {
    fn apply(&self, text: &str, op: &str, args: &Value) -> Result<String, String> {
        let request = serde_json::to_vec(&serde_json::json!({ "text": text, "op": op, "args": args })).map_err(|e| e.to_string())?;
        let answer: Value = serde_json::from_slice(&self.call(&request)?).map_err(|e| format!("The {} add-on answered something gild can't read: {e}", self.name))?;
        match (answer["ok"].as_str(), answer["err"].as_str()) {
            (Some(text), _) => Ok(text.to_string()),
            (_, Some(why)) => Err(why.to_string()),
            _ => Err(format!("The {} add-on answered neither ok nor err.", self.name)),
        }
    }
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

static BUILT_IN: OnceLock<Registry> = OnceLock::new();

impl Registry {
    /// The add-ons that ship with gild, loaded (and their modules compiled)
    /// once.
    pub fn built_in() -> &'static Registry {
        BUILT_IN.get_or_init(|| {
            let markdown = Addon::load(include_str!("../addons/markdown/gild-addon.json"), include_bytes!(env!("GILD_ADDON_MARKDOWN_WASM")))
                .expect("the markdown add-on that ships with gild loads");
            Registry { addons: vec![markdown] }
        })
    }

    pub fn manifests(&self) -> Vec<&Manifest> {
        self.addons.iter().map(|a| &a.manifest).collect()
    }

    /// The add-on whose file type a path is.
    pub fn addon_for(&self, path: &str) -> Option<&Manifest> {
        let t = self.file_type(path)?;
        self.addons.iter().map(|a| &a.manifest).find(|m| m.contributes.file_types.iter().any(|f| f.id == t.id))
    }

    pub fn manifest(&self, id: &str) -> Option<&Manifest> {
        self.addons.iter().map(|a| &a.manifest).find(|m| m.id == id)
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

/// A permission glob: `*` stands for anything (slashes included).
pub fn glob(pattern: &str, path: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == path,
        Some((head, rest)) => {
            path.starts_with(head) && (0..=path.len() - head.len()).any(|i| path.is_char_boundary(head.len() + i) && glob(rest, &path[head.len() + i..]))
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

    /// A module in the WebAssembly text format.
    fn module(wat: &str) -> Vec<u8> {
        wat::parse_str(wat).unwrap()
    }

    const SHELL: &str = r#"(memory (export "memory") 1)
        (func (export "gild_alloc") (param i32) (result i32) (i32.const 1024))"#;

    #[test]
    fn add_ons_run_in_a_sandbox() {
        // A module that asks for anything from outside is refused.
        let importer = module(&format!(r#"(module (import "env" "open" (func)) {SHELL} (func (export "gild_apply") (param i32 i32) (result i64) (i64.const 0)))"#));
        assert!(WasmEditor::new("Sneaky", &importer).err().unwrap().contains("asks for env.open"));
        // One that runs for ever is stopped.
        let spinner = module(&format!(r#"(module {SHELL} (func (export "gild_apply") (param i32 i32) (result i64) (loop (br 0)) (i64.const 0)))"#));
        let e = WasmEditor::new("Spinner", &spinner).unwrap().apply("x", "op", &json!({})).unwrap_err();
        assert_eq!(e, "The Spinner add-on took too long, so gild stopped it.");
        // One that grabs memory past the limit is refused it.
        let hog = module(&format!(r#"(module {SHELL} (func (export "gild_apply") (param i32 i32) (result i64)
            (if (i32.lt_s (memory.grow (i32.const 2000)) (i32.const 0)) (then unreachable)) (i64.const 0)))"#));
        assert!(WasmEditor::new("Hog", &hog).unwrap().apply("x", "op", &json!({})).unwrap_err().starts_with("The Hog add-on failed"));
        // One without the interface is refused.
        assert!(WasmEditor::new("Empty", &module("(module)")).err().unwrap().contains("no `memory`"));
    }

    #[test]
    fn permission_globs() {
        assert!(glob("*.md", "docs/a.md") && glob("*.md", "a.md") && glob("*", "x/y") && glob("docs/*.md", "docs/a.md"));
        assert!(!glob("*.md", "a.csv") && !glob("docs/*.md", "notes/a.md") && !glob("a.md", "b.md"));
        assert!(glob("*é*", "café.md"));
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
