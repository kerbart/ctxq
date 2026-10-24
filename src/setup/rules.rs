//! Marker-delimited block upsert for `AGENTS.md`/`CLAUDE.md`/pointer files: regenerate a
//! single block in place, byte-for-byte stable across reruns, leaving every other line in the
//! file untouched.

pub const BEGIN_MARKER: &str = "<!-- ctxq:begin -->";
pub const END_MARKER: &str = "<!-- ctxq:end -->";

/// The generated `AGENTS.md` block content: a short pointer telling an agent to prefer the
/// `ctxq` MCP tools over blind grepping.
pub fn agents_md_body() -> String {
    "## ctxq\n\n\
     This project is indexed by `ctxq` (MCP server `ctxq`). Use its tools instead of \
     walking the tree or running `ctxq` in a shell (sandboxed agents such as Codex \
     cannot launch it): `search` / `context` for specs and documentation, \
     `code_symbol` / `code_callers` / `code_callees` / `code_path` / `code_impact` \
     for the code graph. If the tools are missing, restart the agent and check `/mcp`."
        .to_string()
}

/// Content of the pointer files (`CLAUDE.md`, `GEMINI.md`, `.github/copilot-instructions.md`):
/// a single line referencing `AGENTS.md`, so instructions live in exactly one place.
pub fn pointer_body(pointer: &str) -> String {
    pointer.to_string()
}

/// Idempotently installs/regenerates the single marker-delimited block of `body` inside
/// `existing` file content (`None` = file doesn't exist yet). Returns `(new_text, changed)`.
/// Content outside the markers — or the whole file, if the markers aren't present yet — is
/// preserved verbatim; a fresh block is appended in that case.
pub fn upsert_block(existing: Option<&str>, body: &str) -> (String, bool) {
    let block = format!("{BEGIN_MARKER}\n{body}\n{END_MARKER}");
    match existing {
        None => (format!("{block}\n"), true),
        Some(text) => {
            if let (Some(start), Some(end_marker_at)) =
                (text.find(BEGIN_MARKER), text.find(END_MARKER))
            {
                if end_marker_at > start {
                    let before = &text[..start];
                    let after = &text[end_marker_at + END_MARKER.len()..];
                    let new_text = format!("{before}{block}{after}");
                    let changed = new_text != text;
                    return (new_text, changed);
                }
            }
            let separator = if text.is_empty() || text.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            let new_text = format!("{text}{separator}\n{block}\n");
            (new_text, true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_a_fresh_file_when_none_existed() {
        let (text, changed) = upsert_block(None, "hello");
        assert!(changed);
        assert_eq!(text, format!("{BEGIN_MARKER}\nhello\n{END_MARKER}\n"));
    }

    #[test]
    fn appends_to_existing_content_without_markers() {
        let (text, changed) = upsert_block(Some("# My project\n\nSome notes.\n"), "hello");
        assert!(changed);
        assert!(text.starts_with("# My project\n\nSome notes.\n"));
        assert!(text.contains(BEGIN_MARKER));
        assert!(text.ends_with(&format!("{END_MARKER}\n")));
    }

    #[test]
    fn replaces_only_the_content_between_existing_markers() {
        let existing = format!("before\n{BEGIN_MARKER}\nold body\n{END_MARKER}\nafter\n");
        let (text, changed) = upsert_block(Some(&existing), "new body");
        assert!(changed);
        assert!(text.starts_with("before\n"));
        assert!(text.contains("new body"));
        assert!(!text.contains("old body"));
        assert!(text.ends_with("after\n"));
    }

    #[test]
    fn rerunning_with_identical_body_reports_no_change() {
        let (first, _) = upsert_block(None, "hello");
        let (second, changed) = upsert_block(Some(&first), "hello");
        assert!(!changed);
        assert_eq!(first, second);
    }

    #[test]
    fn pointer_files_hold_a_single_at_reference() {
        assert_eq!(pointer_body("@AGENTS.md"), "@AGENTS.md");
        assert_eq!(pointer_body("@./AGENTS.md"), "@./AGENTS.md");
    }

    #[test]
    fn agents_block_tells_sandboxed_agents_to_use_mcp() {
        let body = agents_md_body();
        assert!(body.contains("MCP server `ctxq`"));
        assert!(body.contains("Codex"));
        assert!(body.contains("/mcp"));
        assert!(body.contains("shell"));
    }
}
