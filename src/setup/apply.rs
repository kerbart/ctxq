//! Per-client orchestration: resolve the config path, merge or remove the entry, and write a
//! backup when (and only when) content actually changes. Each function takes its base/backup
//! directories and timestamp as parameters — no cache-dir resolution or clock reads here.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::backup::write_with_backup;
use super::clients::{build_entry, config_path, ClientSpec, ConfigFormat};
use super::merge_json;
use super::merge_toml;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Installed { backup: Option<PathBuf> },
    AlreadyUpToDate,
    Uninstalled { backup: Option<PathBuf> },
    NothingToUninstall,
    Rejected(String),
}

fn json_entry_to_toml_entry(entry: &Value) -> merge_toml::Entry {
    let command = entry
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let args = entry
        .get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let env = entry
        .get("env")
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    merge_toml::Entry { command, args, env }
}

/// Installs or updates `spec`'s entry (or removes it, if `uninstall`), under `base_dir`
/// (workspace root for `Project`-scope clients, home/user config dir for `User`-scope ones).
/// `repo_root` is the absolute path pinned into `User`-scope entries (ignored for `Project`
/// scope). Backups go to `backup_dir` tagged with `timestamp`, only on an actual write.
#[allow(clippy::too_many_arguments)]
pub fn apply_client(
    spec: &ClientSpec,
    base_dir: &Path,
    backup_dir: &Path,
    timestamp: &str,
    command: &str,
    repo_root: Option<&str>,
    uninstall: bool,
) -> Result<Outcome, String> {
    let path = config_path(spec, base_dir);
    let original_text = if path.is_file() {
        Some(
            fs::read_to_string(&path)
                .map_err(|e| format!("lecture de {} : {e}", path.display()))?,
        )
    } else {
        None
    };

    if uninstall && original_text.is_none() {
        return Ok(Outcome::NothingToUninstall);
    }

    let entry = if uninstall {
        None
    } else {
        Some(build_entry(spec, command, repo_root))
    };

    let (changed, new_text) = match spec.format {
        ConfigFormat::Json => {
            match merge_json::apply(original_text.as_deref(), spec.root_key_path, entry) {
                Ok(v) => v,
                Err(e) => return Ok(Outcome::Rejected(e)),
            }
        }
        ConfigFormat::Toml => {
            let toml_entry = entry.as_ref().map(json_entry_to_toml_entry);
            match merge_toml::apply(
                original_text.as_deref(),
                spec.root_key_path,
                toml_entry.as_ref(),
            ) {
                Ok(v) => v,
                Err(e) => return Ok(Outcome::Rejected(e)),
            }
        }
    };

    if !changed {
        return Ok(if uninstall {
            Outcome::NothingToUninstall
        } else {
            Outcome::AlreadyUpToDate
        });
    }

    let backup =
        write_with_backup(&path, &new_text, backup_dir, timestamp).map_err(|e| e.to_string())?;
    Ok(if uninstall {
        Outcome::Uninstalled { backup }
    } else {
        Outcome::Installed { backup }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::clients::client_by_id;

    #[test]
    fn installs_a_fresh_json_client_and_writes_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        let spec = client_by_id("cursor").unwrap();
        let outcome = apply_client(
            spec,
            dir.path(),
            &dir.path().join("backups"),
            "ts",
            "documentum",
            None,
            false,
        )
        .unwrap();
        assert!(matches!(outcome, Outcome::Installed { backup: None }));
        let text = fs::read_to_string(config_path(spec, dir.path())).unwrap();
        assert!(text.contains("\"documentum\""));
    }

    #[test]
    fn reinstalling_unchanged_reports_already_up_to_date() {
        let dir = tempfile::tempdir().unwrap();
        let spec = client_by_id("cursor").unwrap();
        let backups = dir.path().join("backups");
        apply_client(spec, dir.path(), &backups, "ts1", "documentum", None, false).unwrap();
        let outcome =
            apply_client(spec, dir.path(), &backups, "ts2", "documentum", None, false).unwrap();
        assert_eq!(outcome, Outcome::AlreadyUpToDate);
        assert!(!backups.exists(), "no-op rerun must not write a backup");
    }

    #[test]
    fn changing_command_backs_up_the_previous_content() {
        let dir = tempfile::tempdir().unwrap();
        let spec = client_by_id("cursor").unwrap();
        let backups = dir.path().join("backups");
        apply_client(
            spec,
            dir.path(),
            &backups,
            "ts1",
            "documentum-old",
            None,
            false,
        )
        .unwrap();
        let outcome = apply_client(
            spec,
            dir.path(),
            &backups,
            "ts2",
            "documentum-new",
            None,
            false,
        )
        .unwrap();
        assert!(matches!(outcome, Outcome::Installed { backup: Some(_) }));
        assert!(backups.read_dir().unwrap().next().is_some());
    }

    #[test]
    fn uninstall_without_prior_install_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let spec = client_by_id("cursor").unwrap();
        let outcome = apply_client(
            spec,
            dir.path(),
            &dir.path().join("backups"),
            "ts",
            "documentum",
            None,
            true,
        )
        .unwrap();
        assert_eq!(outcome, Outcome::NothingToUninstall);
    }

    #[test]
    fn uninstall_removes_only_our_entry() {
        let dir = tempfile::tempdir().unwrap();
        let spec = client_by_id("cursor").unwrap();
        let backups = dir.path().join("backups");
        apply_client(spec, dir.path(), &backups, "ts1", "documentum", None, false).unwrap();
        let config = config_path(spec, dir.path());
        let mut root: serde_json::Map<String, Value> =
            serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        root["mcpServers"]["other-tool"] = serde_json::json!({"command": "foo"});
        fs::write(&config, serde_json::to_string_pretty(&root).unwrap()).unwrap();

        let outcome =
            apply_client(spec, dir.path(), &backups, "ts2", "documentum", None, true).unwrap();
        assert!(matches!(outcome, Outcome::Uninstalled { .. }));
        let text = fs::read_to_string(&config).unwrap();
        assert!(!text.contains("\"documentum\""));
        assert!(text.contains("other-tool"));
    }

    #[test]
    fn user_scope_client_pins_the_repo_path() {
        let dir = tempfile::tempdir().unwrap();
        let spec = client_by_id("windsurf").unwrap();
        apply_client(
            spec,
            dir.path(),
            &dir.path().join("backups"),
            "ts",
            "documentum",
            Some("/abs/repo"),
            false,
        )
        .unwrap();
        let text = fs::read_to_string(config_path(spec, dir.path())).unwrap();
        assert!(text.contains("--path"));
        assert!(text.contains("/abs/repo"));
    }

    #[test]
    fn codex_toml_client_round_trips_through_apply_client() {
        let dir = tempfile::tempdir().unwrap();
        let spec = client_by_id("codex").unwrap();
        let outcome = apply_client(
            spec,
            dir.path(),
            &dir.path().join("backups"),
            "ts",
            "documentum",
            Some("/abs/repo"),
            false,
        )
        .unwrap();
        assert!(matches!(outcome, Outcome::Installed { backup: None }));
        let text = fs::read_to_string(config_path(spec, dir.path())).unwrap();
        assert!(text.contains("[mcp_servers.documentum]"));
    }

    #[test]
    fn rejects_invalid_root_without_writing_anything() {
        let dir = tempfile::tempdir().unwrap();
        let spec = client_by_id("cursor").unwrap();
        let config = config_path(spec, dir.path());
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, "[1, 2, 3]").unwrap();
        let outcome = apply_client(
            spec,
            dir.path(),
            &dir.path().join("backups"),
            "ts",
            "documentum",
            None,
            false,
        )
        .unwrap();
        assert!(matches!(outcome, Outcome::Rejected(_)));
        assert_eq!(fs::read_to_string(&config).unwrap(), "[1, 2, 3]");
    }
}
