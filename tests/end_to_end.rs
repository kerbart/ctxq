//! Index, search, explore, changes, then the MCP `serve` binary on stdio.

use std::io::Write;
use std::process::{Command, Stdio};

fn bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_documentum"));
    cmd.env_remove("DOCUMENTUM_CONFIG")
        .env_remove("DOCUMENTUM_EMBEDDINGS");
    cmd
}

fn run(root: &std::path::Path, cache: &std::path::Path, args: &[&str]) -> String {
    let out = bin()
        .arg("--path")
        .arg(root)
        .arg("--embeddings")
        .arg("local")
        .args(args)
        .env("DOCUMENTUM_CACHE_DIR", cache)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn index_search_explore_changes_and_mcp_serve() {
    let ws = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let root = ws.path();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("docs/orders.md"),
        "# Commandes\n\nLa saisie passe par `parse_order`.\n\nMotif unique xylophone.\n",
    )
    .unwrap();
    std::fs::write(
        root.join("docs/notes.md"),
        "# Notes\n\nUn second document, sans symbole.\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/order.rs"),
        "/// Découpe une commande brute.\npub fn parse_order(raw: &str) -> &str {\n    raw.trim()\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/app.ts"),
        "export function submit(raw: string): string {\n    return parse_order(raw);\n}\n",
    )
    .unwrap();

    run(root, cache.path(), &["index"]);

    let search = run(
        root,
        cache.path(),
        &["search", "xylophone", "--json", "--no-daemon", "-l", "5"],
    );
    let search_json: serde_json::Value = serde_json::from_str(&search).unwrap();
    let sources: Vec<&str> = search_json["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|hit| hit["source"].as_str())
        .collect();
    assert!(
        sources.iter().any(|s| s.ends_with("docs/orders.md")),
        "search: {search}"
    );

    let context = run(
        root,
        cache.path(),
        &["context", "xylophone parse_order", "--json"],
    );
    assert!(
        context.contains("docs/orders.md") && context.contains("parse_order"),
        "context should co-retrieve the doc and the symbol:\n{context}"
    );

    let explore = run(
        root,
        cache.path(),
        &["explore", "parse_order", "--depth", "2"],
    );
    assert!(
        explore.contains("submit"),
        "explore should name the caller:\n{explore}"
    );
    assert!(explore.contains("parse_order"), "{explore}");

    let diff = "\
diff --git a/src/order.rs b/src/order.rs
--- a/src/order.rs
+++ b/src/order.rs
@@ -1,4 +1,4 @@
 /// Découpe une commande brute.
-pub fn parse_order(raw: &str) -> &str {
+pub fn parse_order(raw: &str) -> String {
     raw.trim()
 }
";
    let mut child = bin()
        .arg("--path")
        .arg(root)
        .arg("--embeddings")
        .arg("local")
        .args(["changes", "--json"])
        .env("DOCUMENTUM_CACHE_DIR", cache.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(diff.as_bytes())
        .unwrap();
    let changes = child.wait_with_output().unwrap();
    assert!(
        changes.status.success(),
        "changes: {}",
        String::from_utf8_lossy(&changes.stderr)
    );
    let changes_out = String::from_utf8(changes.stdout).unwrap();
    assert!(
        changes_out.contains("submit"),
        "changes should list the caller:\n{changes_out}"
    );
    assert!(
        changes_out.contains("docs/orders.md"),
        "changes should list the doc:\n{changes_out}"
    );

    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"code_symbol","arguments":{"name":"parse_order"}}}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"search","arguments":{"query":"xylophone"}}}"#,
    ];
    let mut serve = bin()
        .arg("--path")
        .arg(root)
        .arg("--embeddings")
        .arg("local")
        .arg("mcp")
        .env("DOCUMENTUM_CACHE_DIR", cache.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = serve.stdin.take().unwrap();
        for req in requests {
            writeln!(stdin, "{req}").unwrap();
        }
    }
    let mcp = serve.wait_with_output().unwrap();
    assert!(
        mcp.status.success(),
        "serve: {}",
        String::from_utf8_lossy(&mcp.stderr)
    );
    let stdout = String::from_utf8(mcp.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines.len(),
        4,
        "one response per request, notification is silent:\n{stdout}\nstderr: {}",
        String::from_utf8_lossy(&mcp.stderr)
    );
    let responses: Vec<serde_json::Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        responses.iter().all(|r| r.get("error").is_none()),
        "{stdout}"
    );
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "documentum");
    assert_eq!(responses[0]["result"]["protocolVersion"], "2024-11-05");
    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"code_symbol"), "{names:?}");
    assert!(names.contains(&"search"), "{names:?}");

    let symbol_text = responses[2]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(responses[2]["result"]["isError"], false, "{symbol_text}");
    assert!(symbol_text.contains("parse_order"), "{symbol_text}");
    let search_text = responses[3]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(responses[3]["result"]["isError"], false, "{search_text}");
    assert!(
        search_text.contains("docs/orders.md"),
        "mcp search: {search_text}"
    );
    // Protocol bytes stay on stdout; the banner is stderr-only.
    assert!(!stdout.contains("documentum serve"));
}
