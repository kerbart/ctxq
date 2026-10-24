//! Per-client catalog for `setup` (§3.1): which config file, in what format, under which
//! JSON-pointer-ish key path, with what entry shape. Pure data + pure helpers — writing the
//! actual CLI subcommand (flag parsing, cache-dir resolution) is out of scope here.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Pinned to a repo only incidentally (the client is launched from inside it); the
    /// written args are just `["mcp"]`.
    Project,
    /// Global/user-level config: never pinned to a repo unless explicitly named, and always
    /// gets `["mcp", "--path", "<repo-absolu>"]`.
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Json,
    Toml,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryShape {
    /// `{"command", "args", "env"}` — Cursor/Gemini/Antigravity/Generic/Windsurf/
    /// Claude Desktop/Codex/Grok (TOML table, same fields).
    Standard,
    /// Claude Code: `{"type": "stdio", "command", "args", "env"}`. `type` is what Copilot
    /// CLI accepts when it also reads the project `.mcp.json`.
    StdioCommand,
    /// Copilot CLI: stdio plus `"tools": ["*"]` in `~/.copilot/mcp-config.json`.
    CopilotStdio,
    /// VS Code: `{"type": "stdio", "command", "args", "env"}`.
    VsCodeStdio,
    /// Zed: `{"source": "custom", "command", "args", "env"}`.
    ZedCustom,
}

#[derive(Debug, Clone, Copy)]
pub struct ClientSpec {
    /// Stable identifier, matches the `--agent` values where the spec names one directly
    /// (`"antigravity-user"` is the one exception: both the project and user-scope
    /// Antigravity entries share the `--agent antigravity` name in the CLI; which one(s) a
    /// given invocation targets is a `setup` command wiring decision, not this module's).
    pub id: &'static str,
    /// Value written into the `CTXQ_CLIENT` env var of the entry (spec §3.1).
    pub env_client_id: &'static str,
    pub scope: Scope,
    pub format: ConfigFormat,
    /// Relative to the injected base directory (workspace root for `Project`, home/user
    /// config directory for `User`). Empty for `claude-desktop`, whose path is OS-dependent
    /// (see [`claude_desktop_relative_path`]).
    pub relative_config_path: &'static str,
    /// Dotted path to the table holding per-server entries, e.g. `&["mcpServers"]`,
    /// `&["servers"]`, `&["context_servers"]`, `&["mcp_servers"]` (TOML). The final
    /// `"ctxq"` key is appended by the merge logic, not included here.
    pub root_key_path: &'static [&'static str],
    pub entry_shape: EntryShape,
    /// Directory (relative to the workspace root) whose presence marks this *project* client
    /// as installed, for `--agent all` auto-detection. `None` for clients detected only by
    /// their own config file already existing, and for every `User`-scope client (never
    /// auto-detected via a project marker). Codex is the exception: see
    /// [`detect_setup_clients`].
    pub detect_marker: Option<&'static str>,
}

impl ClientSpec {
    /// User-scope clients pin `["mcp", "--path", repo]` so a global config still
    /// indexes the repo that ran `setup`. Codex, Copilot and Grok do not: a
    /// pinned path would steal every other project the next time `setup` runs.
    /// Those CLIs launch MCP from the session cwd (and, for Codex and Grok, a
    /// project config that does pin `cwd`).
    pub fn pins_repo_path(&self) -> bool {
        self.scope == Scope::User && !matches!(self.id, "codex-user" | "grok-user" | "copilot")
    }

    /// Project entries for Codex and Grok set `cwd` so the stdio server starts
    /// in the repo even if the client process cwd is elsewhere. Their user-scope
    /// entries must not: that would reintroduce last-project-wins.
    pub fn includes_cwd(&self) -> bool {
        self.scope == Scope::Project && matches!(self.id, "codex" | "grok")
    }
}

/// Every client from the §3.1 table. `"generic"` is deliberately excluded from
/// auto-detection ([`detect_project_clients`]): it is the exact same target as `"claude"`
/// (`.mcp.json` / `mcpServers`), kept only so it can be named explicitly as a project-scope
/// fallback for clients without a dedicated row.
pub const CLIENTS: &[ClientSpec] = &[
    ClientSpec {
        id: "claude",
        env_client_id: "claude-code",
        scope: Scope::Project,
        format: ConfigFormat::Json,
        relative_config_path: ".mcp.json",
        root_key_path: &["mcpServers"],
        entry_shape: EntryShape::StdioCommand,
        detect_marker: None,
    },
    ClientSpec {
        id: "cursor",
        env_client_id: "cursor",
        scope: Scope::Project,
        format: ConfigFormat::Json,
        relative_config_path: ".cursor/mcp.json",
        root_key_path: &["mcpServers"],
        entry_shape: EntryShape::Standard,
        detect_marker: Some(".cursor"),
    },
    ClientSpec {
        id: "vscode",
        env_client_id: "vscode",
        scope: Scope::Project,
        format: ConfigFormat::Json,
        relative_config_path: ".vscode/mcp.json",
        root_key_path: &["servers"],
        entry_shape: EntryShape::VsCodeStdio,
        detect_marker: Some(".vscode"),
    },
    ClientSpec {
        id: "gemini",
        env_client_id: "gemini-cli",
        scope: Scope::Project,
        format: ConfigFormat::Json,
        relative_config_path: ".gemini/settings.json",
        root_key_path: &["mcpServers"],
        entry_shape: EntryShape::Standard,
        detect_marker: Some(".gemini"),
    },
    ClientSpec {
        id: "antigravity",
        env_client_id: "antigravity",
        scope: Scope::Project,
        format: ConfigFormat::Json,
        relative_config_path: ".agents/mcp_config.json",
        root_key_path: &["mcpServers"],
        entry_shape: EntryShape::Standard,
        detect_marker: Some(".agents"),
    },
    ClientSpec {
        id: "zed",
        env_client_id: "zed",
        scope: Scope::Project,
        format: ConfigFormat::Json,
        relative_config_path: ".zed/settings.json",
        root_key_path: &["context_servers"],
        entry_shape: EntryShape::ZedCustom,
        detect_marker: Some(".zed"),
    },
    ClientSpec {
        id: "generic",
        env_client_id: "generic",
        scope: Scope::Project,
        format: ConfigFormat::Json,
        relative_config_path: ".mcp.json",
        root_key_path: &["mcpServers"],
        entry_shape: EntryShape::StdioCommand,
        detect_marker: None,
    },
    ClientSpec {
        id: "antigravity-user",
        env_client_id: "antigravity",
        scope: Scope::User,
        format: ConfigFormat::Json,
        relative_config_path: ".gemini/config/mcp_config.json",
        root_key_path: &["mcpServers"],
        entry_shape: EntryShape::Standard,
        detect_marker: None,
    },
    ClientSpec {
        id: "codex",
        env_client_id: "codex",
        scope: Scope::Project,
        format: ConfigFormat::Toml,
        relative_config_path: ".codex/config.toml",
        root_key_path: &["mcp_servers"],
        entry_shape: EntryShape::Standard,
        detect_marker: Some(".codex"),
    },
    ClientSpec {
        id: "codex-user",
        env_client_id: "codex",
        scope: Scope::User,
        format: ConfigFormat::Toml,
        relative_config_path: ".codex/config.toml",
        root_key_path: &["mcp_servers"],
        entry_shape: EntryShape::Standard,
        detect_marker: None,
    },
    ClientSpec {
        id: "windsurf",
        env_client_id: "windsurf",
        scope: Scope::User,
        format: ConfigFormat::Json,
        relative_config_path: ".codeium/windsurf/mcp_config.json",
        root_key_path: &["mcpServers"],
        entry_shape: EntryShape::Standard,
        detect_marker: None,
    },
    ClientSpec {
        id: "claude-desktop",
        env_client_id: "claude-desktop",
        scope: Scope::User,
        format: ConfigFormat::Json,
        relative_config_path: "",
        root_key_path: &["mcpServers"],
        entry_shape: EntryShape::Standard,
        detect_marker: None,
    },
    ClientSpec {
        id: "copilot",
        env_client_id: "copilot",
        scope: Scope::User,
        format: ConfigFormat::Json,
        relative_config_path: ".copilot/mcp-config.json",
        root_key_path: &["mcpServers"],
        entry_shape: EntryShape::CopilotStdio,
        detect_marker: None,
    },
    ClientSpec {
        id: "grok",
        env_client_id: "grok",
        scope: Scope::Project,
        format: ConfigFormat::Toml,
        relative_config_path: ".grok/config.toml",
        root_key_path: &["mcp_servers"],
        entry_shape: EntryShape::Standard,
        detect_marker: Some(".grok"),
    },
    ClientSpec {
        id: "grok-user",
        env_client_id: "grok",
        scope: Scope::User,
        format: ConfigFormat::Toml,
        relative_config_path: ".grok/config.toml",
        root_key_path: &["mcp_servers"],
        entry_shape: EntryShape::Standard,
        detect_marker: None,
    },
];

pub fn client_by_id(id: &str) -> Option<&'static ClientSpec> {
    CLIENTS.iter().find(|c| c.id == id)
}

/// OS-dependent suffix (relative to the user's home directory) for Claude Desktop's config
/// file. Takes `os` explicitly (rather than reading `std::env::consts::OS` itself) so every
/// branch is directly testable regardless of the host running the tests.
pub fn claude_desktop_relative_path(os: &str) -> &'static str {
    match os {
        "macos" => "Library/Application Support/Claude/claude_desktop_config.json",
        "windows" => "AppData/Roaming/Claude/claude_desktop_config.json",
        _ => ".config/Claude/claude_desktop_config.json",
    }
}

/// Resolves `spec`'s config file path under `base` (workspace root for `Project`, home
/// directory for `User`).
pub fn config_path(spec: &ClientSpec, base: &Path) -> PathBuf {
    if spec.id == "claude-desktop" {
        base.join(claude_desktop_relative_path(std::env::consts::OS))
    } else {
        base.join(spec.relative_config_path)
    }
}

/// Builds the JSON entry for `spec`: `["mcp"]` for a project client, `["mcp", "--path",
/// repo_root]` for a user client that [`ClientSpec::pins_repo_path`] (when `repo_root`
/// is given). Codex project entries also set `cwd`.
pub fn build_entry(spec: &ClientSpec, command: &str, repo_root: Option<&str>) -> Value {
    let mut args = vec![Value::String("mcp".to_string())];
    if spec.pins_repo_path() {
        if let Some(root) = repo_root {
            args.push(Value::String("--path".to_string()));
            args.push(Value::String(root.to_string()));
        }
    }
    let env = json!({ "CTXQ_CLIENT": spec.env_client_id });
    let mut entry = match spec.entry_shape {
        EntryShape::Standard => json!({ "command": command, "args": args, "env": env }),
        EntryShape::StdioCommand => json!({
            "type": "stdio", "command": command, "args": args, "env": env
        }),
        EntryShape::CopilotStdio => json!({
            "type": "stdio", "command": command, "args": args, "env": env, "tools": ["*"]
        }),
        EntryShape::VsCodeStdio => json!({
            "type": "stdio", "command": command, "args": args, "env": env
        }),
        EntryShape::ZedCustom => json!({
            "source": "custom", "command": command, "args": args, "env": env
        }),
    };
    if spec.includes_cwd() {
        if let Some(root) = repo_root {
            entry["cwd"] = json!(root);
        }
    }
    entry
}

/// Project-scope clients whose dedicated marker directory (or, lacking one, own config file)
/// is present under `workspace_root`. User-scope clients and `"generic"` (a pure alias of
/// `"claude"`, see [`CLIENTS`]) are never returned. Prefer [`detect_setup_clients`] for
/// `--client all`: that also picks up Codex when it is installed in `$HOME`.
pub fn detect_project_clients(workspace_root: &Path) -> Vec<&'static ClientSpec> {
    CLIENTS
        .iter()
        .filter(|c| c.scope == Scope::Project && c.id != "generic")
        .filter(|c| {
            let marker_present = c
                .detect_marker
                .map(|m| workspace_root.join(m).is_dir())
                .unwrap_or(false);
            marker_present || config_path(c, workspace_root).is_file()
        })
        .collect()
}

/// One row of the CLI catalog. `auto` families are registered by a plain
/// `ctxq setup` when the CLI is installed (home marker or binary on `PATH`).
/// The others stay opt-in (`--client` or `--interactive`) unless a project
/// marker already selects them.
pub struct CliFamily {
    pub id: &'static str,
    pub label: &'static str,
    pub detail: &'static str,
    pub client_ids: &'static [&'static str],
    pub home_markers: &'static [&'static str],
    pub binaries: &'static [&'static str],
    pub auto: bool,
}

pub const CLI_FAMILIES: &[CliFamily] = &[
    CliFamily {
        id: "claude",
        label: "Claude Code",
        detail: ".mcp.json",
        client_ids: &["claude"],
        home_markers: &[".claude", ".claude.json"],
        binaries: &["claude"],
        auto: true,
    },
    CliFamily {
        id: "copilot",
        label: "Copilot",
        detail: "~/.copilot/mcp-config.json",
        client_ids: &["copilot"],
        home_markers: &[".copilot"],
        binaries: &["copilot"],
        auto: true,
    },
    CliFamily {
        id: "codex",
        label: "Codex",
        detail: ".codex/config.toml + ~/.codex/config.toml",
        client_ids: &["codex", "codex-user"],
        home_markers: &[".codex"],
        binaries: &["codex"],
        auto: true,
    },
    CliFamily {
        id: "grok",
        label: "Grok",
        detail: ".grok/config.toml + ~/.grok/config.toml",
        client_ids: &["grok", "grok-user"],
        home_markers: &[".grok"],
        binaries: &["grok"],
        auto: true,
    },
    CliFamily {
        id: "gemini",
        label: "Gemini CLI",
        detail: ".gemini/settings.json",
        client_ids: &["gemini"],
        home_markers: &[".gemini"],
        binaries: &["gemini"],
        auto: true,
    },
    CliFamily {
        id: "cursor",
        label: "Cursor",
        detail: ".cursor/mcp.json",
        client_ids: &["cursor"],
        home_markers: &[".cursor"],
        binaries: &["cursor"],
        auto: false,
    },
    CliFamily {
        id: "vscode",
        label: "VS Code",
        detail: ".vscode/mcp.json",
        client_ids: &["vscode"],
        home_markers: &[".vscode"],
        binaries: &["code"],
        auto: false,
    },
    CliFamily {
        id: "zed",
        label: "Zed",
        detail: ".zed/settings.json",
        client_ids: &["zed"],
        home_markers: &[".zed"],
        binaries: &["zed"],
        auto: false,
    },
    CliFamily {
        id: "antigravity",
        label: "Antigravity",
        detail: ".agents/mcp_config.json",
        client_ids: &["antigravity"],
        home_markers: &[],
        binaries: &["agy", "antigravity"],
        auto: false,
    },
    CliFamily {
        id: "antigravity-user",
        label: "Antigravity user",
        detail: "~/.gemini/config (pins this repo)",
        client_ids: &["antigravity-user"],
        home_markers: &[".gemini"],
        binaries: &["agy", "antigravity"],
        auto: false,
    },
    CliFamily {
        id: "windsurf",
        label: "Windsurf",
        detail: "~/.codeium/windsurf (pins this repo)",
        client_ids: &["windsurf"],
        home_markers: &[".codeium/windsurf"],
        binaries: &["windsurf"],
        auto: false,
    },
    CliFamily {
        id: "claude-desktop",
        label: "Claude Desktop",
        detail: "Claude Desktop config (pins this repo)",
        client_ids: &["claude-desktop"],
        home_markers: &[".config/Claude", "Library/Application Support/Claude"],
        binaries: &[],
        auto: false,
    },
];

/// Clients `setup` registers by default: project clients present in the
/// workspace, plus every [`CLI_FAMILIES`] entry with `auto` whose CLI is
/// installed (home marker or binary on `PATH`).
///
/// Codex cannot run `ctxq` in its shell sandbox. MCP registration is the only
/// path that works out of the box, so it is one of the auto CLIs.
pub fn detect_setup_clients(
    workspace_root: &Path,
    home: Option<&Path>,
) -> Vec<&'static ClientSpec> {
    detect_setup_clients_with(workspace_root, home, path_has_bin)
}

pub(crate) fn detect_setup_clients_with(
    workspace_root: &Path,
    home: Option<&Path>,
    has_bin: impl Fn(&str) -> bool,
) -> Vec<&'static ClientSpec> {
    let mut out = detect_project_clients(workspace_root);
    for family in CLI_FAMILIES {
        if !family.auto || !family_installed(family, home, &has_bin) {
            continue;
        }
        for id in family.client_ids {
            if let Some(spec) = client_by_id(id) {
                push_unique(&mut out, spec);
            }
        }
    }
    out
}

pub(crate) fn path_has_bin(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

pub(crate) fn family_installed(
    family: &CliFamily,
    home: Option<&Path>,
    has_bin: &impl Fn(&str) -> bool,
) -> bool {
    if family.binaries.iter().any(|bin| has_bin(bin)) {
        return true;
    }
    let Some(home) = home else {
        return false;
    };
    family.home_markers.iter().any(|marker| {
        let path = home.join(marker);
        path.is_file() || path.is_dir()
    })
}

fn push_unique<'a>(out: &mut Vec<&'a ClientSpec>, spec: &'a ClientSpec) {
    if !out.iter().any(|s| s.id == spec.id) {
        out.push(spec);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_is_an_alias_of_claude_same_file_same_keys() {
        let claude = client_by_id("claude").unwrap();
        let generic = client_by_id("generic").unwrap();
        assert_eq!(claude.relative_config_path, generic.relative_config_path);
        assert_eq!(claude.root_key_path, generic.root_key_path);
    }

    #[test]
    fn user_scope_clients_are_never_auto_detectable() {
        for spec in CLIENTS.iter().filter(|c| c.scope == Scope::User) {
            assert!(
                spec.detect_marker.is_none(),
                "{} must not be auto-detected",
                spec.id
            );
        }
    }

    #[test]
    fn claude_desktop_path_differs_per_os() {
        assert!(claude_desktop_relative_path("macos").contains("Library"));
        assert!(claude_desktop_relative_path("windows").contains("AppData"));
        assert!(claude_desktop_relative_path("linux").starts_with(".config"));
    }

    #[test]
    fn project_entry_has_no_path_argument() {
        let spec = client_by_id("cursor").unwrap();
        let entry = build_entry(spec, "ctxq", Some("/repo"));
        assert_eq!(entry["args"], json!(["mcp"]));
        assert_eq!(entry["env"]["CTXQ_CLIENT"], "cursor");
    }

    #[test]
    fn user_entry_pins_the_repo_path() {
        let spec = client_by_id("windsurf").unwrap();
        let entry = build_entry(spec, "ctxq", Some("/abs/repo"));
        assert_eq!(entry["args"], json!(["mcp", "--path", "/abs/repo"]));
    }

    #[test]
    fn vscode_entry_has_the_stdio_type_field() {
        let spec = client_by_id("vscode").unwrap();
        let entry = build_entry(spec, "ctxq", None);
        assert_eq!(entry["type"], "stdio");
    }

    #[test]
    fn zed_entry_has_the_custom_source_field() {
        let spec = client_by_id("zed").unwrap();
        let entry = build_entry(spec, "ctxq", None);
        assert_eq!(entry["source"], "custom");
    }

    #[test]
    fn detect_project_clients_finds_only_markers_present_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".cursor")).unwrap();
        let found = detect_project_clients(dir.path());
        let ids: Vec<&str> = found.iter().map(|c| c.id).collect();
        assert!(ids.contains(&"cursor"));
        assert!(!ids.contains(&"zed"));
        assert!(!ids.contains(&"generic"));
    }

    #[test]
    fn detect_project_clients_finds_claude_by_its_own_config_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".mcp.json"), "{}").unwrap();
        let ids: Vec<&str> = detect_project_clients(dir.path())
            .iter()
            .map(|c| c.id)
            .collect();
        assert!(ids.contains(&"claude"));
    }

    #[test]
    fn detect_project_clients_finds_codex_by_its_marker() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".codex")).unwrap();
        let ids: Vec<&str> = detect_project_clients(dir.path())
            .iter()
            .map(|c| c.id)
            .collect();
        assert!(ids.contains(&"codex"));
        assert!(!ids.contains(&"codex-user"));
    }

    #[test]
    fn codex_project_entry_sets_cwd_and_does_not_pin_path() {
        let spec = client_by_id("codex").unwrap();
        assert_eq!(spec.scope, Scope::Project);
        let entry = build_entry(spec, "ctxq", Some("/abs/repo"));
        assert_eq!(entry["args"], json!(["mcp"]));
        assert_eq!(entry["cwd"], "/abs/repo");
        assert_eq!(entry["env"]["CTXQ_CLIENT"], "codex");
    }

    #[test]
    fn codex_user_entry_does_not_pin_the_repo_path() {
        let spec = client_by_id("codex-user").unwrap();
        assert_eq!(spec.scope, Scope::User);
        let entry = build_entry(spec, "ctxq", Some("/abs/repo"));
        assert_eq!(entry["args"], json!(["mcp"]));
        assert!(entry.get("cwd").is_none());
    }

    #[test]
    fn detect_setup_clients_includes_codex_when_home_has_dot_codex() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".codex")).unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let ids: Vec<&str> =
            detect_setup_clients_with(workspace.path(), Some(home.path()), |_| false)
                .iter()
                .map(|c| c.id)
                .collect();
        assert!(ids.contains(&"codex"));
        assert!(ids.contains(&"codex-user"));
    }

    #[test]
    fn detect_setup_clients_skips_codex_when_not_installed() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let ids: Vec<&str> =
            detect_setup_clients_with(workspace.path(), Some(home.path()), |_| false)
                .iter()
                .map(|c| c.id)
                .collect();
        assert!(!ids.contains(&"codex"));
        assert!(!ids.contains(&"codex-user"));
    }

    #[test]
    fn claude_entry_declares_stdio_so_copilot_can_read_the_same_file() {
        let spec = client_by_id("claude").unwrap();
        let entry = build_entry(spec, "ctxq", Some("/repo"));
        assert_eq!(entry["type"], "stdio");
        assert_eq!(entry["args"], json!(["mcp"]));
        assert!(entry.get("tools").is_none());
    }

    #[test]
    fn copilot_entry_is_stdio_with_all_tools_and_no_pinned_path() {
        let spec = client_by_id("copilot").unwrap();
        assert!(!spec.pins_repo_path());
        let entry = build_entry(spec, "/usr/bin/ctxq", Some("/repo"));
        assert_eq!(entry["type"], "stdio");
        assert_eq!(entry["args"], json!(["mcp"]));
        assert_eq!(entry["tools"], json!(["*"]));
        assert_eq!(entry["env"]["CTXQ_CLIENT"], "copilot");
    }

    #[test]
    fn grok_project_sets_cwd_and_user_follows_the_session() {
        let project = client_by_id("grok").unwrap();
        let entry = build_entry(project, "ctxq", Some("/abs/repo"));
        assert_eq!(entry["args"], json!(["mcp"]));
        assert_eq!(entry["cwd"], "/abs/repo");
        let user = client_by_id("grok-user").unwrap();
        assert!(!user.pins_repo_path());
        let entry = build_entry(user, "ctxq", Some("/abs/repo"));
        assert_eq!(entry["args"], json!(["mcp"]));
        assert!(entry.get("cwd").is_none());
    }

    #[test]
    fn auto_clis_register_from_a_home_marker_or_a_binary() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".copilot")).unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let ids: Vec<&str> =
            detect_setup_clients_with(workspace.path(), Some(home.path()), |bin| bin == "grok")
                .iter()
                .map(|c| c.id)
                .collect();
        assert!(ids.contains(&"copilot"));
        assert!(ids.contains(&"grok"));
        assert!(ids.contains(&"grok-user"));
        assert!(!ids.contains(&"vscode"));
        assert!(!ids.contains(&"claude"));
    }
}
