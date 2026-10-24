//! Idempotent TOML config merge for Codex and Grok (§3.1): navigate/
//! create the `root_key_path` table chain with `toml_edit`, preserving comments/formatting of
//! everything else in the document, and upsert or remove the `ctxq` sub-table.

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table};

/// The fields of our own entry, independent of any JSON `Value` shape — built once by the
/// caller (from the same logical data as [`crate::setup::clients::build_entry`]) and handed
/// to this module so it never needs to know about `serde_json`.
#[derive(Debug, Clone)]
pub struct Entry {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Working directory for the stdio process (Codex `[mcp_servers.*].cwd`).
    pub cwd: Option<String>,
}

fn entry_to_table(entry: &Entry) -> Table {
    let mut table = Table::new();
    table["command"] = toml_edit::value(entry.command.clone());
    let mut args = Array::new();
    for arg in &entry.args {
        args.push(arg.as_str());
    }
    table["args"] = Item::Value(args.into());
    let mut env = InlineTable::new();
    for (key, value) in &entry.env {
        env.insert(key, value.as_str().into());
    }
    table["env"] = Item::Value(env.into());
    if let Some(cwd) = &entry.cwd {
        table["cwd"] = toml_edit::value(cwd.clone());
    }
    table
}

/// Walks `root_key_path` inside `doc`, creating empty tables for missing intermediate keys.
/// Fails without modifying `doc` if an existing intermediate key isn't itself a table.
fn navigate<'a>(doc: &'a mut DocumentMut, root_key_path: &[&str]) -> Result<&'a mut Table, String> {
    for key in root_key_path {
        if let Some(existing) = doc.as_table().get(key) {
            if !existing.is_table() {
                return Err(format!("key \"{key}\" already exists and is not a table"));
            }
        }
    }
    let mut current: &mut Table = doc.as_table_mut();
    for key in root_key_path {
        if !current.contains_key(key) {
            current.insert(key, Item::Table(Table::new()));
        }
        current = current
            .get_mut(key)
            .and_then(Item::as_table_mut)
            .expect("validated above");
    }
    Ok(current)
}

/// Applies the full merge/removal to `original_text` (`None` meaning the file doesn't exist
/// yet) and returns `(changed, new_text)`. `entry = None` means uninstall.
pub fn apply(
    original_text: Option<&str>,
    root_key_path: &[&str],
    entry: Option<&Entry>,
) -> Result<(bool, String), String> {
    let mut doc: DocumentMut = match original_text {
        Some(text) if !text.trim().is_empty() => {
            text.parse().map_err(|e| format!("TOML invalide : {e}"))?
        }
        _ => DocumentMut::new(),
    };
    {
        let table = navigate(&mut doc, root_key_path)?;
        match entry {
            Some(e) => {
                table.insert("ctxq", Item::Table(entry_to_table(e)));
            }
            None => {
                table.remove("ctxq");
            }
        }
    }
    let new_text = doc.to_string();
    let changed = original_text != Some(new_text.as_str());
    Ok((changed, new_text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> Entry {
        Entry {
            command: "ctxq".to_string(),
            args: vec!["mcp".to_string(), "--path".to_string(), "/repo".to_string()],
            env: vec![("CTXQ_CLIENT".to_string(), "codex".to_string())],
            cwd: None,
        }
    }

    #[test]
    fn writes_cwd_when_set() {
        let mut entry = sample_entry();
        entry.args = vec!["mcp".to_string()];
        entry.cwd = Some("/abs/repo".to_string());
        let (_, text) = apply(None, &["mcp_servers"], Some(&entry)).unwrap();
        assert!(text.contains("cwd = \"/abs/repo\""));
        assert!(!text.contains("--path"));
    }

    #[test]
    fn merge_keeps_unrelated_tables() {
        let original = "[cli]\ninstaller = \"internal\"\n";
        let mut entry = sample_entry();
        entry.args = vec!["mcp".to_string()];
        entry.env = vec![("CTXQ_CLIENT".to_string(), "grok".to_string())];
        let (changed, text) = apply(Some(original), &["mcp_servers"], Some(&entry)).unwrap();
        assert!(changed);
        assert!(text.contains("[cli]"));
        assert!(text.contains("installer = \"internal\""));
        assert!(text.contains("[mcp_servers.ctxq]"));
        assert!(text.contains("command = \"ctxq\""));
        assert!(!text.contains("--path"));
    }

    #[test]
    fn creates_missing_table_chain_on_absent_file() {
        let (changed, text) = apply(None, &["mcp_servers"], Some(&sample_entry())).unwrap();
        assert!(changed);
        assert!(text.contains("[mcp_servers.ctxq]"));
        assert!(text.contains("command = \"ctxq\""));
    }

    #[test]
    fn preserves_sibling_tables_and_comments() {
        let original = "# user comment\n[mcp_servers.other-tool]\ncommand = \"foo\"\n";
        let (_, text) = apply(Some(original), &["mcp_servers"], Some(&sample_entry())).unwrap();
        assert!(text.contains("# user comment"));
        assert!(text.contains("[mcp_servers.other-tool]"));
        assert!(text.contains("command = \"foo\""));
        assert!(text.contains("[mcp_servers.ctxq]"));
    }

    #[test]
    fn refuses_when_intermediate_key_is_not_a_table() {
        let original = "mcp_servers = \"oops\"\n";
        let err = apply(Some(original), &["mcp_servers"], Some(&sample_entry())).unwrap_err();
        assert!(err.contains("mcp_servers"));
    }

    #[test]
    fn is_idempotent_rerunning_with_the_same_entry_reports_no_change() {
        let (_, first) = apply(None, &["mcp_servers"], Some(&sample_entry())).unwrap();
        let (changed, second) =
            apply(Some(&first), &["mcp_servers"], Some(&sample_entry())).unwrap();
        assert!(!changed);
        assert_eq!(first, second);
    }

    #[test]
    fn removes_only_our_table_leaving_siblings_intact() {
        let (_, installed) = apply(None, &["mcp_servers"], Some(&sample_entry())).unwrap();
        let with_other = format!("{installed}[mcp_servers.other-tool]\ncommand = \"foo\"\n");
        let (changed, text) = apply(Some(&with_other), &["mcp_servers"], None).unwrap();
        assert!(changed);
        assert!(!text.contains("[mcp_servers.ctxq]"));
        assert!(text.contains("[mcp_servers.other-tool]"));
    }

    #[test]
    fn uninstall_on_file_without_our_entry_reports_no_change() {
        let original = "[mcp_servers.other-tool]\ncommand = \"foo\"\n";
        let (changed, _) = apply(Some(original), &["mcp_servers"], None).unwrap();
        assert!(!changed);
    }
}
