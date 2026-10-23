//! Token-to-character budgeting shared by every tool that bounds its text output.
//! §4.3: "budget de tokens ≈ 4 car./token".

use serde_json::Value;

pub const CHARS_PER_TOKEN: usize = 4;

/// Converts a token budget into a character budget using the spec's fixed ratio.
/// `0` tokens means "no budget" (unbounded) by convention of callers.
pub fn tokens_to_chars(max_tokens: u32) -> usize {
    (max_tokens as usize).saturating_mul(CHARS_PER_TOKEN)
}

/// Truncates `text` to at most `max_chars` **characters** (not bytes, to stay UTF-8 safe).
/// `max_chars == 0` disables truncation entirely. Returns the (possibly unchanged) text and
/// whether truncation actually happened.
pub fn truncate_chars(text: &str, max_chars: usize) -> (String, bool) {
    if max_chars == 0 {
        return (text.to_string(), false);
    }
    let total = text.chars().count();
    if total <= max_chars {
        return (text.to_string(), false);
    }
    (text.chars().take(max_chars).collect(), true)
}

/// Appends a human-readable truncation notice, mirroring the spec's requirement to always
/// "flag the truncation" rather than silently cutting output.
pub fn with_truncation_notice(mut text: String, truncated: bool, hint: &str) -> String {
    if truncated {
        text.push_str("\n\n[truncated: ");
        text.push_str(hint);
        text.push(']');
    }
    text
}

/// Truncates `lines` (already selected) to fit within `max_chars`, returning the kept text,
/// how many trailing lines were omitted, and whether anything was cut.
pub fn truncate_lines(lines: &[&str], max_chars: usize) -> (String, usize) {
    if max_chars == 0 {
        return (lines.join("\n"), 0);
    }
    let mut kept = Vec::new();
    let mut used = 0usize;
    for (i, line) in lines.iter().enumerate() {
        let next_used = used + line.chars().count() + usize::from(i > 0);
        if next_used > max_chars && !kept.is_empty() {
            return (kept.join("\n"), lines.len() - kept.len());
        }
        kept.push(*line);
        used = next_used;
    }
    (kept.join("\n"), 0)
}

/// Keeps as many JSON values as fit within `max_chars` (serialised size), in order, never
/// splitting a value in two. Returns the kept values, how many were omitted, and whether
/// anything was cut. `max_chars == 0` disables the budget (everything is kept).
pub fn bound_values(items: Vec<Value>, max_chars: usize) -> (Vec<Value>, usize, bool) {
    if max_chars == 0 {
        return (items, 0, false);
    }
    let mut kept = Vec::new();
    let mut used = 2usize; // "[]"
    for item in &items {
        let added = item.to_string().len() + 1; // + comma/sep
        if used + added > max_chars && !kept.is_empty() {
            let omitted = items.len() - kept.len();
            return (kept, omitted, true);
        }
        kept.push(item.clone());
        used += added;
    }
    (kept, 0, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_tokens_to_chars_at_fixed_ratio() {
        assert_eq!(tokens_to_chars(500), 2000);
        assert_eq!(tokens_to_chars(0), 0);
    }

    #[test]
    fn truncate_chars_is_noop_under_budget() {
        let (text, truncated) = truncate_chars("hello", 100);
        assert_eq!(text, "hello");
        assert!(!truncated);
    }

    #[test]
    fn truncate_chars_cuts_on_unicode_boundaries() {
        let (text, truncated) = truncate_chars("héllo wörld", 3);
        assert_eq!(text, "hél");
        assert!(truncated);
    }

    #[test]
    fn with_truncation_notice_only_appends_when_truncated() {
        assert_eq!(with_truncation_notice("x".into(), false, "hint"), "x");
        assert!(with_truncation_notice("x".into(), true, "hint").contains("hint"));
    }

    #[test]
    fn truncate_lines_keeps_whole_lines_within_budget() {
        let lines = vec!["one", "two", "three"];
        // "one" (3) + "\n" (1) + "two" (3) = 7 fits exactly; "three" would push past budget.
        let (kept, omitted) = truncate_lines(&lines, 7);
        assert_eq!(kept, "one\ntwo");
        assert_eq!(omitted, 1);
    }

    #[test]
    fn truncate_lines_keeps_all_when_budget_is_zero() {
        let lines = vec!["one", "two"];
        let (kept, omitted) = truncate_lines(&lines, 0);
        assert_eq!(kept, "one\ntwo");
        assert_eq!(omitted, 0);
    }

    #[test]
    fn bound_values_keeps_everything_under_budget() {
        let items = vec![Value::from(1), Value::from(2)];
        let (kept, omitted, truncated) = bound_values(items.clone(), 1000);
        assert_eq!(kept, items);
        assert_eq!(omitted, 0);
        assert!(!truncated);
    }

    #[test]
    fn bound_values_always_keeps_at_least_one_item() {
        let items = vec![Value::from("x".repeat(50)), Value::from("y")];
        let (kept, omitted, truncated) = bound_values(items, 5);
        assert_eq!(kept.len(), 1);
        assert_eq!(omitted, 1);
        assert!(truncated);
    }

    #[test]
    fn bound_values_zero_budget_keeps_all() {
        let items = vec![Value::from(1), Value::from(2), Value::from(3)];
        let (kept, omitted, truncated) = bound_values(items.clone(), 0);
        assert_eq!(kept, items);
        assert_eq!(omitted, 0);
        assert!(!truncated);
    }
}
