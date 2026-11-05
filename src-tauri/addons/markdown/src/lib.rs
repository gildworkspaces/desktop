//! gild's Markdown add-on, as a WebAssembly module.
//!
//! The app runs it in a sandbox: no imports at all (so no files, network or
//! clock), with limits on memory and on how long a call may run. The whole
//! interface is two functions, the same for every add-on editor:
//!
//! - `gild_alloc(len) -> ptr`: room for the host to write a request.
//! - `gild_apply(ptr, len) -> (ptr << 32) | len`: the request is JSON
//!   `{"text", "op", "args"}`; the answer is JSON `{"ok": <new text>}` or
//!   `{"err": <why>}`.

use serde_json::{json, Value};

#[no_mangle]
pub extern "C" fn gild_alloc(len: u32) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len as usize);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// # Safety
/// `ptr` and `len` are a request the host wrote into memory from `gild_alloc`.
#[no_mangle]
pub unsafe extern "C" fn gild_apply(ptr: *mut u8, len: u32) -> u64 {
    let request = std::slice::from_raw_parts(ptr, len as usize);
    let answer = match serde_json::from_slice::<Value>(request) {
        Ok(r) => match apply(r["text"].as_str().unwrap_or(""), r["op"].as_str().unwrap_or(""), &r["args"]) {
            Ok(text) => json!({ "ok": text }),
            Err(e) => json!({ "err": e }),
        },
        Err(e) => json!({ "err": format!("the request isn't JSON: {e}") }),
    };
    let mut out = serde_json::to_vec(&answer).unwrap_or_default().into_boxed_slice();
    let (p, n) = (out.as_mut_ptr() as u64, out.len() as u64);
    std::mem::forget(out);
    (p << 32) | n
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&level).then(|| line[level..].strip_prefix(' ')).flatten().map(|t| (level, t.trim()))
}

fn arg<'a>(args: &'a Value, name: &str) -> &'a str {
    args.get(name).and_then(Value::as_str).unwrap_or("")
}

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

/// One operation on a markdown file's text.
pub fn apply(text: &str, op: &str, args: &Value) -> Result<String, String> {
    let lines: Vec<&str> = text.lines().collect();
    match op {
        "replace_section" => {
            let (start, end) = section(&lines, arg(args, "heading"))?;
            Ok(join(&lines[..=start], arg(args, "text"), &lines[end..]))
        }
        "insert_after" => {
            let (start, end) = section(&lines, arg(args, "heading"))?;
            let body = lines[start + 1..end].join("\n");
            let body = body.trim_matches('\n');
            let added = arg(args, "text").trim_matches('\n');
            let middle = if body.is_empty() { added.to_string() } else { format!("{body}\n\n{added}") };
            Ok(join(&lines[..=start], &middle, &lines[end..]))
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
