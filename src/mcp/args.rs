//! Small `serde_json::Value` argument-extraction helpers shared by every tool handler.

use serde_json::Value;

/// A required string argument, or an explanatory error (tool error, not a protocol one).
pub fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing or empty required string argument \"{key}\""))
}

/// An optional string argument; absent, non-string, or empty all map to `None`.
pub fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// An optional unsigned integer argument, falling back to `default` when absent or of the
/// wrong type.
pub fn opt_u32(args: &Value, key: &str, default: u32) -> u32 {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(default)
}

/// An optional array-of-strings argument, defaulting to an empty vector.
pub fn opt_str_array(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Clamps `depth` into `[1, max]`, the pattern used by every depth-bounded tool (§4.3).
pub fn clamp_depth(depth: u32, max: u32) -> u32 {
    depth.clamp(1, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn require_str_rejects_missing_and_empty() {
        let args = json!({ "name": "" });
        assert!(require_str(&args, "name").is_err());
        assert!(require_str(&args, "missing").is_err());
    }

    #[test]
    fn require_str_accepts_present_value() {
        let args = json!({ "name": "foo" });
        assert_eq!(require_str(&args, "name").unwrap(), "foo");
    }

    #[test]
    fn opt_u32_falls_back_on_wrong_type_or_absence() {
        let args = json!({ "depth": "not a number" });
        assert_eq!(opt_u32(&args, "depth", 7), 7);
        assert_eq!(opt_u32(&json!({}), "depth", 7), 7);
        assert_eq!(opt_u32(&json!({"depth": 3}), "depth", 7), 3);
    }

    #[test]
    fn clamp_depth_bounds_into_range() {
        assert_eq!(clamp_depth(0, 3), 1);
        assert_eq!(clamp_depth(99, 3), 3);
        assert_eq!(clamp_depth(2, 3), 2);
    }

    #[test]
    fn opt_str_array_extracts_strings_and_skips_non_strings() {
        let args = json!({ "tags": ["a", 1, "b"] });
        assert_eq!(
            opt_str_array(&args, "tags"),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(opt_str_array(&json!({}), "tags").is_empty());
    }
}
