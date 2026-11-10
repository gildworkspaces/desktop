//! The MCP clients on this computer that gild can set up for an agent, so
//! nobody pastes a command: gild finds which are installed, which already
//! know the agent, and adds or removes it through each client's own CLI.
//!
//! Each agent is one MCP server in a client, named `gild-<agent>`, pointing
//! at gild's endpoint with the agent's token in an Authorization header.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::wake::run_command;

const STEP: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    ClaudeCode,
    Codex,
    /// Pi (pi.dev), the agent harness that also runs local models: how an
    /// agent on this computer's own model, like Ava, works in gild.
    Pi,
}

pub const KINDS: [Kind; 3] = [Kind::ClaudeCode, Kind::Codex, Kind::Pi];

impl Kind {
    pub fn id(self) -> &'static str {
        match self {
            Kind::ClaudeCode => "claude-code",
            Kind::Codex => "codex",
            Kind::Pi => "pi",
        }
    }
    fn name(self) -> &'static str {
        match self {
            Kind::ClaudeCode => "Claude Code",
            Kind::Codex => "Codex",
            Kind::Pi => "Pi",
        }
    }
}

/// Where the clients are: their programs and the home folder their config
/// lives in. The app uses the person's; tests use stand-ins.
pub struct Clients {
    pub claude: String,
    pub codex: String,
    pub pi: String,
    pub home: PathBuf,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct ClientStatus {
    pub id: &'static str,
    pub name: &'static str,
    pub installed: bool,
    /// The client already has this agent's server.
    pub connected: bool,
    /// What gild runs to wake the agent through this client.
    pub wake: String,
}

/// `'…'`, for a POSIX shell.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub fn server_name(agent: &str) -> String {
    format!("gild-{agent}")
}

/// The command that wakes `agent` through a client: it reads the prompt on
/// stdin and may use only the agent's gild tools.
pub fn wake_command(kind: Kind, agent: &str) -> String {
    match kind {
        Kind::ClaudeCode => format!("claude -p --allowedTools mcp__{}", server_name(agent)),
        Kind::Codex => "codex exec --skip-git-repo-check -".into(),
        // Only the agent's gild tools (Pi's built-in file and shell tools are
        // left out), no AGENTS.md from the folder it wakes in, and no saved
        // session: gild sends the conversation with every wake.
        Kind::Pi => format!("pi -p --no-session --no-context-files --tools {}", quote(&format!("{}*", pi_tool_prefix(agent)))),
    }
}

/// Pi names an MCP server's tools `mcp__<server>__<tool>`, with every
/// character other than a letter, digit or `_` in the server's name made `_`.
fn pi_tool_prefix(agent: &str) -> String {
    let server: String = server_name(agent).chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    format!("mcp__{server}__")
}

/// Pi's user-level MCP servers, where `pi mcp add` puts them.
fn pi_config(home: &Path) -> PathBuf {
    home.join(".pi").join("agent").join("mcp.json")
}

/// The address and Authorization header Pi has for `name`, if it has it.
fn pi_server(home: &Path, name: &str) -> Option<(String, String)> {
    let text = std::fs::read_to_string(pi_config(home)).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let server = &json["mcpServers"][name];
    Some((server["url"].as_str()?.to_string(), server["headers"]["Authorization"].as_str().unwrap_or_default().to_string()))
}

impl Clients {
    pub fn for_this_computer() -> Clients {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_else(|| "/".into());
        Clients { claude: "claude".into(), codex: "codex".into(), pi: "pi".into(), home }
    }

    fn program(&self, kind: Kind) -> &str {
        match kind {
            Kind::ClaudeCode => &self.claude,
            Kind::Codex => &self.codex,
            Kind::Pi => &self.pi,
        }
    }

    async fn run(&self, kind: Kind, args: &str) -> Result<String, String> {
        run_command(&format!("{} {args}", quote(self.program(kind))), "", STEP).await
    }

    async fn installed(&self, kind: Kind) -> bool {
        run_command(&format!("command -v {}", quote(self.program(kind))), "", STEP).await.is_ok()
    }

    async fn connected(&self, kind: Kind, agent: &str) -> bool {
        let name = quote(&server_name(agent));
        match kind {
            Kind::ClaudeCode => self.run(kind, &format!("mcp get {name}")).await.is_ok(),
            Kind::Codex => self.run(kind, &format!("mcp get {name} --json")).await.is_ok(),
            // `pi mcp list` connects to every server it has; reading its
            // config answers without that.
            Kind::Pi => pi_server(&self.home, &server_name(agent)).is_some(),
        }
    }

    /// Every client gild knows, whether it's here, and whether it knows `agent`.
    pub async fn status(&self, agent: &str) -> Vec<ClientStatus> {
        let mut out = Vec::new();
        for kind in KINDS {
            let installed = self.installed(kind).await;
            let connected = installed && self.connected(kind, agent).await;
            out.push(ClientStatus { id: kind.id(), name: kind.name(), installed, connected, wake: wake_command(kind, agent) });
        }
        out
    }

    /// Take the agent's server out of a client (fine if it isn't there).
    pub async fn disconnect(&self, kind: Kind, agent: &str) {
        match kind {
            Kind::ClaudeCode => {
                let _ = self.run(kind, &format!("mcp remove --scope user {}", quote(&server_name(agent)))).await;
            }
            Kind::Codex => {
                let config = codex_config(&self.home);
                if let Ok(text) = std::fs::read_to_string(&config) {
                    let kept = without_server(&text, &server_name(agent));
                    if kept != text {
                        let _ = std::fs::write(&config, kept);
                    }
                }
            }
            Kind::Pi => {
                let _ = self.run(kind, &format!("mcp remove {}", quote(&server_name(agent)))).await;
            }
        }
    }

    /// Give a client the agent's server, replacing any it had (an old one
    /// carries a token that no longer works).
    pub async fn connect(&self, kind: Kind, agent: &str, endpoint: &str, token: &str) -> Result<(), String> {
        self.disconnect(kind, agent).await;
        let name = server_name(agent);
        let fail = |e: String| format!("{} couldn't add {name}: {e}", kind.name());
        match kind {
            Kind::ClaudeCode => {
                // User scope: found from any folder, including the home
                // folder gild wakes agents in.
                let header = format!("Authorization: Bearer {token}");
                self.run(kind, &format!("mcp add --scope user --transport http {} {} --header {}", quote(&name), quote(endpoint), quote(&header)))
                    .await
                    .map_err(fail)?;
            }
            Kind::Codex => {
                // Codex's CLI takes a header only from an environment
                // variable, and rewrites the whole config when it adds a
                // server. gild adds its own two tables to the end instead, so
                // the rest of the file stays as the person wrote it, and
                // then asks Codex to read them back.
                let config = codex_config(&self.home);
                let mut text = match std::fs::read_to_string(&config) {
                    Ok(t) => t,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(e) => return Err(fail(format!("reading {}: {e}", config.display()))),
                };
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                if !text.is_empty() {
                    text.push('\n');
                }
                let key = toml_key(&name);
                text.push_str(&format!(
                    "[mcp_servers.{key}]\nurl = {}\n\n[mcp_servers.{key}.http_headers]\nAuthorization = {}\n",
                    toml_string(endpoint),
                    toml_string(&format!("Bearer {token}"))
                ));
                if let Some(dir) = config.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| fail(format!("making {}: {e}", dir.display())))?;
                }
                std::fs::write(&config, text).map_err(|e| fail(format!("writing {}: {e}", config.display())))?;
                let got = self.run(kind, &format!("mcp get {} --json", quote(&name))).await.map_err(fail)?;
                let json: serde_json::Value = serde_json::from_str(&got).map_err(|e| fail(format!("reading its answer: {e}")))?;
                if json["transport"]["url"].as_str() != Some(endpoint) || json["transport"]["http_headers"]["Authorization"].as_str() != Some(&format!("Bearer {token}")) {
                    return Err(fail("it doesn't read back the address and token gild wrote".into()));
                }
            }
            Kind::Pi => {
                // A user-level server, found from any folder. Pi writes it to
                // its mcp.json without connecting, so gild reads it back.
                let header = format!("Authorization=Bearer {token}");
                self.run(kind, &format!("mcp add {} --url {} --header {}", quote(&name), quote(endpoint), quote(&header))).await.map_err(fail)?;
                if pi_server(&self.home, &name) != Some((endpoint.to_string(), format!("Bearer {token}"))) {
                    return Err(fail("it doesn't read back the address and token gild gave it".into()));
                }
            }
        }
        if !self.connected(kind, agent).await {
            return Err(fail("it doesn't list it afterwards".into()));
        }
        Ok(())
    }
}

/// The config without `name`'s tables (`[mcp_servers.<name>]` and its
/// sub-tables); everything else exactly as it was.
fn without_server(text: &str, name: &str) -> String {
    let key = toml_key(name);
    let (own, sub) = (format!("[mcp_servers.{key}]"), format!("[mcp_servers.{key}."));
    let mut out = String::new();
    let mut skipping = false;
    let mut removed = false;
    for line in text.split_inclusive('\n') {
        let t = line.trim();
        if t.starts_with('[') {
            skipping = t == own || t.starts_with(&sub);
            removed |= skipping;
        }
        if !skipping {
            out.push_str(line);
        }
    }
    if removed {
        // The blank line gild put before its tables goes with them.
        while out.ends_with("\n\n") {
            out.pop();
        }
    }
    out
}

/// A TOML basic string.
fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn codex_config(home: &Path) -> PathBuf {
    home.join(".codex").join("config.toml")
}

/// A TOML key: bare when it can be, quoted otherwise.
fn toml_key(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        s.to_string()
    } else {
        toml_string(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(path: &Path, body: &str) -> String {
        std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path.display().to_string()
    }

    /// A stand-in for `claude mcp add/get/remove`: one file per server.
    fn fake_claude(dir: &Path) -> String {
        let store = dir.join("claude-servers");
        std::fs::create_dir_all(&store).unwrap();
        script(
            &dir.join("claude"),
            &format!(
                r#"S='{}'
[ "$1" = mcp ] || exit 2
cmd=$2; shift 2
while [ "${{1#--}}" != "$1" ]; do shift 2; done
n=$1; shift
case $cmd in
  add) printf '%s\n' "$*" > "$S/$n" ;;
  get) [ -f "$S/$n" ] || {{ echo "No MCP server named $n" >&2; exit 1; }} ;;
  remove) [ -f "$S/$n" ] || exit 1; rm "$S/$n" ;;
esac
"#,
                store.display()
            ),
        )
    }

    /// A stand-in for `codex mcp get <name> --json`: reads the server's url
    /// and Authorization header out of config.toml, and answers at the
    /// length the real one does (longer than a line of output).
    fn fake_codex(dir: &Path) -> String {
        let config = codex_config(dir);
        script(
            &dir.join("codex"),
            &format!(
                r#"C='{}'
[ "$1 $2" = "mcp get" ] || exit 2
n=$3
grep -qxF "[mcp_servers.$n]" "$C" 2>/dev/null || {{ echo "No MCP server named '$n' found." >&2; exit 1; }}
u=$(awk -v s="[mcp_servers.$n]" '$0==s{{f=1;next}} /^\[/{{f=0}} f&&/^url = /{{sub(/^url = "/,"");sub(/"$/,"");print}}' "$C")
h=$(awk -v s="[mcp_servers.$n.http_headers]" '$0==s{{f=1;next}} /^\[/{{f=0}} f&&/^Authorization = /{{sub(/^Authorization = "/,"");sub(/"$/,"");print}}' "$C")
printf '{{\n  "name": "%s",\n  "enabled": true,\n  "disabled_reason": null,\n  "transport": {{\n    "type": "streamable_http",\n    "url": "%s",\n    "bearer_token_env_var": null,\n    "http_headers": {{\n      "Authorization": "%s"\n    }},\n    "env_http_headers": null,\n    "http_headers_helper": null\n  }},\n  "enabled_tools": null,\n  "disabled_tools": null,\n  "startup_timeout_sec": null,\n  "tool_timeout_sec": null\n}}\n' "$n" "$u" "$h"
"#,
                config.display()
            ),
        )
    }

    /// A stand-in for `pi mcp add/remove`: keeps one file per server and
    /// writes `~/.pi/agent/mcp.json` from them in the shape the real Pi
    /// writes (`{"mcpServers":{"<name>":{"url":…,"headers":{…}}}}`), without
    /// connecting; removing a server it doesn't have fails, as Pi's does.
    fn fake_pi(dir: &Path) -> String {
        let store = dir.join("pi-servers");
        std::fs::create_dir_all(&store).unwrap();
        script(
            &dir.join("pi"),
            &format!(
                r#"S='{}'; C='{}'
[ "$1" = mcp ] || exit 2
cmd=$2; n=$3; shift 3
case $cmd in
  add) u=; h=
       while [ $# -gt 0 ]; do case $1 in --url) u=$2; shift 2;; --header) h=${{2#Authorization=}}; shift 2;; *) shift;; esac; done
       printf '%s\n%s\n' "$u" "$h" > "$S/$n" ;;
  remove) [ -f "$S/$n" ] || {{ echo "No global MCP server named \"$n\"" >&2; exit 1; }}; rm "$S/$n" ;;
  *) exit 2 ;;
esac
mkdir -p "$(dirname "$C")"
{{ printf '{{"mcpServers":{{'; sep=
  for f in "$S"/*; do [ -f "$f" ] || continue
    printf '%s"%s":{{"url":"%s","headers":{{"Authorization":"%s"}}}}' "$sep" "$(basename "$f")" "$(sed -n 1p "$f")" "$(sed -n 2p "$f")"; sep=,
  done; printf '}}}}\n'; }} > "$C"
"#,
                store.display(),
                pi_config(dir).display()
            ),
        )
    }

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gild-clients-{}", crate::store::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Someone's Codex config, as they wrote it: comments, spacing, order.
    const MINE: &str = "# my settings\nmodel = \"gpt-5\"\n\n[mcp_servers.Roblox_Studio]\nargs = []\ncommand = \"/Applications/RobloxStudio.app/Contents/MacOS/StudioMCP\"\nstartup_timeout_sec = 120\n\n[projects.\"/Volumes/Projects\"]\ntrust_level = \"trusted\"\n";

    #[tokio::test]
    async fn gild_finds_the_clients_and_sets_the_agent_up_in_them() {
        let dir = temp();
        let clients = Clients { claude: fake_claude(&dir), codex: fake_codex(&dir), pi: fake_pi(&dir), home: dir.clone() };
        std::fs::create_dir_all(dir.join(".codex")).unwrap();
        std::fs::write(codex_config(&dir), MINE).unwrap();
        let endpoint = "http://127.0.0.1:4747/mcp";

        let before = clients.status("ava").await;
        assert_eq!(
            before.iter().map(|c| (c.id, c.installed, c.connected)).collect::<Vec<_>>(),
            [("claude-code", true, false), ("codex", true, false), ("pi", true, false)]
        );
        assert_eq!(before[0].wake, "claude -p --allowedTools mcp__gild-ava");
        assert_eq!(before[2].wake, "pi -p --no-session --no-context-files --tools 'mcp__gild_ava__*'");

        clients.connect(Kind::ClaudeCode, "ava", endpoint, "gild_one").await.unwrap();
        clients.connect(Kind::Codex, "ava", endpoint, "gild_one").await.unwrap();
        clients.connect(Kind::Pi, "ava", endpoint, "gild_one").await.unwrap();
        let added = std::fs::read_to_string(dir.join("claude-servers/gild-ava")).unwrap();
        assert_eq!(added.trim(), "http://127.0.0.1:4747/mcp --header Authorization: Bearer gild_one");
        let ours = "[mcp_servers.gild-ava]\nurl = \"http://127.0.0.1:4747/mcp\"\n\n[mcp_servers.gild-ava.http_headers]\nAuthorization = \"Bearer gild_one\"\n";
        assert_eq!(std::fs::read_to_string(codex_config(&dir)).unwrap(), format!("{MINE}\n{ours}"), "the person's config untouched, gild's tables after it");
        assert_eq!(pi_server(&dir, "gild-ava"), Some((endpoint.to_string(), "Bearer gild_one".to_string())));
        assert!(clients.status("ava").await.iter().all(|c| c.connected));
        assert!(clients.status("intake").await.iter().all(|c| !c.connected), "only the agent it was set up for");

        // Connecting again replaces the server: the new token, once.
        clients.connect(Kind::ClaudeCode, "ava", endpoint, "gild_two").await.unwrap();
        clients.connect(Kind::Codex, "ava", endpoint, "gild_two").await.unwrap();
        clients.connect(Kind::Pi, "ava", endpoint, "gild_two").await.unwrap();
        assert!(std::fs::read_to_string(dir.join("claude-servers/gild-ava")).unwrap().contains("Bearer gild_two"));
        assert_eq!(std::fs::read_to_string(codex_config(&dir)).unwrap(), format!("{MINE}\n{}", ours.replace("gild_one", "gild_two")));
        assert_eq!(pi_server(&dir, "gild-ava"), Some((endpoint.to_string(), "Bearer gild_two".to_string())));

        clients.disconnect(Kind::ClaudeCode, "ava").await;
        assert_eq!(clients.status("ava").await.iter().map(|c| c.connected).collect::<Vec<_>>(), [false, true, true]);
        clients.disconnect(Kind::Codex, "ava").await;
        assert_eq!(std::fs::read_to_string(codex_config(&dir)).unwrap(), MINE, "byte for byte as it was");
        clients.disconnect(Kind::Pi, "ava").await;
        assert_eq!(pi_server(&dir, "gild-ava"), None);
        assert!(clients.status("ava").await.iter().all(|c| !c.connected));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn codex_without_a_config_yet_gets_one() {
        let dir = temp();
        let clients = Clients { claude: fake_claude(&dir), codex: fake_codex(&dir), pi: fake_pi(&dir), home: dir.clone() };
        clients.connect(Kind::Codex, "ava", "http://127.0.0.1:4747/mcp", "gild_x").await.unwrap();
        assert!(std::fs::read_to_string(codex_config(&dir)).unwrap().starts_with("[mcp_servers.gild-ava]\n"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_client_that_isn_t_installed_says_so() {
        let dir = temp();
        let nothing = dir.join("nothing-here").display().to_string();
        let clients = Clients { claude: nothing.clone(), codex: fake_codex(&dir), pi: nothing, home: dir.clone() };
        let status = clients.status("ava").await;
        assert_eq!(status.iter().map(|c| (c.id, c.installed)).collect::<Vec<_>>(), [("claude-code", false), ("codex", true), ("pi", false)]);
        let err = clients.connect(Kind::ClaudeCode, "ava", "http://127.0.0.1:4747/mcp", "gild_x").await.unwrap_err();
        assert!(err.starts_with("Claude Code couldn't add gild-ava"), "{err}");
        let err = clients.connect(Kind::Pi, "ava", "http://127.0.0.1:4747/mcp", "gild_x").await.unwrap_err();
        assert!(err.starts_with("Pi couldn't add gild-ava"), "{err}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn quoting() {
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(toml_key("gild-ava"), "gild-ava");
        assert_eq!(toml_key("gild-é"), "\"gild-é\"");
        // Pi's tool names: the server's name with anything but letters,
        // digits and `_` made `_`.
        assert_eq!(pi_tool_prefix("ava"), "mcp__gild_ava__");
        assert_eq!(pi_tool_prefix("north-side é"), "mcp__gild_north_side____");
        // Only the agent's own tables go, not another server sharing a prefix.
        let text = "[mcp_servers.gild-ava]\nurl = \"a\"\n\n[mcp_servers.gild-ava.http_headers]\nx = 1\n[mcp_servers.gild-ava2]\nurl = \"b\"\n";
        assert_eq!(without_server(text, "gild-ava"), "[mcp_servers.gild-ava2]\nurl = \"b\"\n");
    }
}
