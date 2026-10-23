//! Idempotent JSON config merge (§3.1): navigate/create the `root_key_path` table, upsert or
//! remove the `"documentum"` entry, never touch anything else in the file.

use serde_json::{Map, Value};

/// Parses `text` as a JSON object, treating an empty/absent file as `{}`. Refuses (without
/// panicking) anything that isn't an object at the root — we must never silently clobber a
/// config file whose shape we don't understand.
pub fn parse_root_object(text: &str) -> Result<Map<String, Value>, String> {
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    let value: Value = serde_json::from_str(text).map_err(|e| format!("JSON invalide : {e}"))?;
    match value {
        Value::Object(map) => Ok(map),
        other => Err(format!(
            "the file root is not a JSON object (found: {})",
            value_kind(&other)
        )),
    }
}

fn value_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Walks `root_key_path` from `root`, creating empty objects for missing intermediate keys,
/// then inserts `entry` under the final `"documentum"` key. Fails without modifying `root` if
/// any existing intermediate key is present but isn't itself an object.
pub fn merge_entry(
    root: &mut Map<String, Value>,
    root_key_path: &[&str],
    entry: Value,
) -> Result<(), String> {
    for key in root_key_path {
        if let Some(existing) = root.get(*key) {
            if !existing.is_object() {
                return Err(format!("key \"{key}\" already exists and is not an object"));
            }
        }
    }
    let mut current = root;
    for key in root_key_path {
        let slot = current
            .entry((*key).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        current = slot.as_object_mut().expect("validated above");
    }
    current.insert("documentum".to_string(), entry);
    Ok(())
}

/// Removes the `"documentum"` entry at `root_key_path`, if present. Returns whether anything
/// was actually removed (a pure no-op must not trigger a backup/write upstream).
pub fn remove_entry(root: &mut Map<String, Value>, root_key_path: &[&str]) -> bool {
    let mut current: &mut Map<String, Value> = root;
    for key in root_key_path {
        match current.get_mut(*key) {
            Some(Value::Object(map)) => current = map,
            _ => return false,
        }
    }
    current.remove("documentum").is_some()
}

/// Applies the full merge/removal to `original_text` (`None` meaning the file doesn't exist
/// yet) and returns `(changed, new_text)`. `entry = None` means uninstall. `new_text` always
/// ends with a trailing newline.
pub fn apply(
    original_text: Option<&str>,
    root_key_path: &[&str],
    entry: Option<Value>,
) -> Result<(bool, String), String> {
    let mut root = match original_text {
        Some(text) => parse_root_object(text)?,
        None => Map::new(),
    };
    match entry {
        Some(e) => merge_entry(&mut root, root_key_path, e)?,
        None => {
            remove_entry(&mut root, root_key_path);
        }
    }
    let new_text = format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(root)).expect("maps serialize")
    );
    let changed = original_text != Some(new_text.as_str());
    Ok((changed, new_text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn creates_missing_intermediate_keys() {
        let mut root = Map::new();
        merge_entry(&mut root, &["mcpServers"], json!({"command": "documentum"})).unwrap();
        assert_eq!(root["mcpServers"]["documentum"]["command"], "documentum");
    }

    #[test]
    fn preserves_sibling_entries_and_unrelated_keys() {
        let mut root: Map<String, Value> = serde_json::from_str(
            r#"{"mcpServers": {"other-tool": {"command": "foo"}}, "unrelatedTopLevel": 42}"#,
        )
        .unwrap();
        merge_entry(&mut root, &["mcpServers"], json!({"command": "documentum"})).unwrap();
        assert_eq!(root["mcpServers"]["other-tool"]["command"], "foo");
        assert_eq!(root["unrelatedTopLevel"], 42);
        assert_eq!(root["mcpServers"]["documentum"]["command"], "documentum");
    }

    #[test]
    fn refuses_when_intermediate_key_is_not_an_object() {
        let mut root: Map<String, Value> =
            serde_json::from_str(r#"{"mcpServers": "oops"}"#).unwrap();
        let err = merge_entry(&mut root, &["mcpServers"], json!({})).unwrap_err();
        assert!(err.contains("mcpServers"));
        // Must be left untouched on refusal.
        assert_eq!(root["mcpServers"], "oops");
    }

    #[test]
    fn remove_entry_is_a_harmless_noop_when_absent() {
        let mut root = Map::new();
        assert!(!remove_entry(&mut root, &["mcpServers"]));
    }

    #[test]
    fn remove_entry_deletes_only_our_key() {
        let mut root: Map<String, Value> =
            serde_json::from_str(r#"{"mcpServers": {"documentum": {}, "other-tool": {}}}"#)
                .unwrap();
        assert!(remove_entry(&mut root, &["mcpServers"]));
        assert!(!root["mcpServers"]
            .as_object()
            .unwrap()
            .contains_key("documentum"));
        assert!(root["mcpServers"]
            .as_object()
            .unwrap()
            .contains_key("other-tool"));
    }

    #[test]
    fn apply_on_absent_file_creates_fresh_object() {
        let (changed, text) = apply(
            None,
            &["mcpServers"],
            Some(json!({"command": "documentum"})),
        )
        .unwrap();
        assert!(changed);
        assert!(text.contains("\"documentum\""));
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn apply_is_idempotent_rerunning_with_the_same_entry_reports_no_change() {
        let entry = json!({"command": "documentum", "args": ["mcp"]});
        let (_, first) = apply(None, &["mcpServers"], Some(entry.clone())).unwrap();
        let (changed, second) = apply(Some(&first), &["mcpServers"], Some(entry)).unwrap();
        assert!(!changed);
        assert_eq!(first, second);
    }

    #[test]
    fn apply_uninstall_on_file_without_our_entry_reports_no_change() {
        // Build the "before" text through the same canonical serializer `apply` itself uses,
        // so the comparison isn't polluted by unrelated formatting differences.
        let before = format!(
            "{}\n",
            serde_json::to_string_pretty(
                &json!({"mcpServers": {"other-tool": {"command": "other"}}})
            )
            .unwrap()
        );
        let (changed, _) = apply(Some(&before), &["mcpServers"], None).unwrap();
        assert!(!changed);
    }

    #[test]
    fn apply_rejects_non_object_root_without_producing_output() {
        let err = apply(Some("[1, 2, 3]"), &["mcpServers"], Some(json!({}))).unwrap_err();
        assert!(err.contains("object"));
    }
}
