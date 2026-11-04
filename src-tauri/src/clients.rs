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
}

pub const KINDS: [Kind; 2] = [Kind::ClaudeCode, Kind::Codex];

impl Kind {
    pub fn id(self) -> &'static str {
        match self {
            Kind::ClaudeCode => "claude-code",
            Kind::Codex => "codex",
        }
    }
    fn name(self) -> &'static str {
        match self {
            Kind::ClaudeCode => "Claude Code",
            Kind::Codex => "Codex",
        }
    }
}

/// Where the clients are: their programs and the home folder their config
/// lives in. The app uses the person's; tests use stand-ins.
pub struct Clients {
    pub claude: String,
    pub codex: String,
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
    }
}

impl Clients {
    pub fn for_this_computer() -> Clients {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_else(|| "/".into());
        Clients { claude: "claude".into(), codex: "codex".into(), home }
    }

    fn program(&self, kind: Kind) -> &str {
        match kind {
            Kind::ClaudeCode => &self.claude,
            Kind::Codex => &self.codex,
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
        let name = quote(&server_name(agent));
        let args = match kind {
            Kind::ClaudeCode => format!("mcp remove --scope user {name}"),
            Kind::Codex => format!("mcp remove {name}"),
        };
        let _ = self.run(kind, &args).await;
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
                // variable, so the header goes into its config next to the
                // server the CLI added.
                self.run(kind, &format!("mcp add {} --url {}", quote(&name), quote(endpoint))).await.map_err(fail)?;
                let config = codex_config(&self.home);
                let mut text = std::fs::read_to_string(&config).map_err(|e| fail(format!("reading {}: {e}", config.display())))?;
                if !text.ends_with('\n') && !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!("\n[mcp_servers.{}.http_headers]\nAuthorization = \"Bearer {token}\"\n", toml_key(&name)));
                std::fs::write(&config, text).map_err(|e| fail(format!("writing {}: {e}", config.display())))?;
                // Codex reads it back with the header, or it isn't set up.
                let got = self.run(kind, &format!("mcp get {} --json", quote(&name))).await.map_err(fail)?;
                let json: serde_json::Value = serde_json::from_str(&got).map_err(|e| fail(format!("reading its answer: {e}")))?;
                if json["transport"]["http_headers"]["Authorization"].as_str() != Some(&format!("Bearer {token}")) {
                    return Err(fail("it didn't keep the token header".into()));
                }
            }
        }
        if !self.connected(kind, agent).await {
            return Err(fail("it doesn't list it afterwards".into()));
        }
        Ok(())
    }
}

fn codex_config(home: &Path) -> PathBuf {
    home.join(".codex").join("config.toml")
}

/// A TOML key: bare when it can be, quoted otherwise.
fn toml_key(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        s.to_string()
    } else {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for a client's CLI: `mcp add/get/remove` over one file
    /// per server, the way the real ones behave from outside. The Codex one
    /// writes `[mcp_servers.<name>]` to config.toml like codex does, and
    /// `get --json` reports the header found under it.
    fn fake(dir: &Path, name: &str, codex: bool) -> String {
        let path = dir.join(name);
        let store = dir.join(format!("{name}-servers"));
        std::fs::create_dir_all(&store).unwrap();
        let config = codex_config(dir);
        let script = format!(
            r#"#!/bin/sh
S='{store}'
[ "$1" = mcp ] || exit 2
cmd=$2; shift 2
while [ "${{1#--}}" != "$1" ]; do shift 2; done
n=$1; shift
case $cmd in
  add) printf '%s\n' "$*" > "$S/$n"; {codex_add} ;;
  get) [ -f "$S/$n" ] || {{ echo "No MCP server named $n" >&2; exit 1; }}; {codex_get} ;;
  remove) [ -f "$S/$n" ] || exit 1; rm "$S/$n" ;;
esac
"#,
            store = store.display(),
            codex_add = if codex { format!("mkdir -p '{0}'; printf '[mcp_servers.%s]\\nurl = \"%s\"\\n' \"$n\" \"$2\" >> '{1}'", config.parent().unwrap().display(), config.display()) } else { String::new() },
            codex_get = if codex {
                format!("h=$(sed -n 's/^Authorization = \"\\(.*\\)\"$/\\1/p' '{}' | tail -1); printf '{{\"transport\":{{\"http_headers\":{{\"Authorization\":\"%s\"}}}}}}' \"$h\"", config.display())
            } else {
                String::new()
            },
        );
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path.display().to_string()
    }

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gild-clients-{}", crate::store::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn gild_finds_the_clients_and_sets_the_agent_up_in_them() {
        let dir = temp();
        let clients = Clients { claude: fake(&dir, "claude", false), codex: fake(&dir, "codex", true), home: dir.clone() };
        let endpoint = "http://127.0.0.1:4747/mcp";

        let before = clients.status("ava").await;
        assert_eq!(before.iter().map(|c| (c.id, c.installed, c.connected)).collect::<Vec<_>>(), [("claude-code", true, false), ("codex", true, false)]);
        assert_eq!(before[0].wake, "claude -p --allowedTools mcp__gild-ava");

        clients.connect(Kind::ClaudeCode, "ava", endpoint, "gild_one").await.unwrap();
        clients.connect(Kind::Codex, "ava", endpoint, "gild_one").await.unwrap();
        let added = std::fs::read_to_string(dir.join("claude-servers/gild-ava")).unwrap();
        assert_eq!(added.trim(), "http://127.0.0.1:4747/mcp --header Authorization: Bearer gild_one");
        let config = std::fs::read_to_string(codex_config(&dir)).unwrap();
        assert!(config.contains("[mcp_servers.gild-ava]\nurl = \"http://127.0.0.1:4747/mcp\"\n\n[mcp_servers.gild-ava.http_headers]\nAuthorization = \"Bearer gild_one\"\n"), "{config}");
        assert!(clients.status("ava").await.iter().all(|c| c.connected));
        assert!(clients.status("intake").await.iter().all(|c| !c.connected), "only the agent it was set up for");

        // Connecting again replaces the server: the new token, once.
        clients.connect(Kind::ClaudeCode, "ava", endpoint, "gild_two").await.unwrap();
        assert!(std::fs::read_to_string(dir.join("claude-servers/gild-ava")).unwrap().contains("Bearer gild_two"));

        clients.disconnect(Kind::ClaudeCode, "ava").await;
        assert_eq!(clients.status("ava").await.iter().map(|c| c.connected).collect::<Vec<_>>(), [false, true]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_client_that_isn_t_installed_says_so() {
        let dir = temp();
        let clients = Clients { claude: dir.join("nothing-here").display().to_string(), codex: fake(&dir, "codex", true), home: dir.clone() };
        let status = clients.status("ava").await;
        assert_eq!(status.iter().map(|c| (c.id, c.installed)).collect::<Vec<_>>(), [("claude-code", false), ("codex", true)]);
        let err = clients.connect(Kind::ClaudeCode, "ava", "http://127.0.0.1:4747/mcp", "gild_x").await.unwrap_err();
        assert!(err.starts_with("Claude Code couldn't add gild-ava"), "{err}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn quoting() {
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(toml_key("gild-ava"), "gild-ava");
        assert_eq!(toml_key("gild-é"), "\"gild-é\"");
    }
}
