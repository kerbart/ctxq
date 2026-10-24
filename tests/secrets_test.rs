//! P2 gate: no secret ever reaches the index (trap set).

use ctxq::core::hybrid::DocEngine;
use ctxq::core::parse_issues::{list, KIND_QUARANTINED};

fn sync(engine: &mut DocEngine, root: &std::path::Path) {
    engine.sync_directory_incremental(root, None).unwrap();
}

fn chunks_of(engine: &DocEngine, source: &str) -> i64 {
    engine
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM doc_chunks WHERE source_path = ?1",
            [source],
            |r| r.get(0),
        )
        .unwrap()
}

fn all_text(engine: &DocEngine) -> String {
    let mut stmt = engine
        .conn()
        .prepare("SELECT content FROM doc_chunks")
        .unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.map(|r| r.unwrap()).collect::<Vec<_>>().join("\n")
}

#[test]
fn secrets_are_quarantined_and_released_once_removed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let gh = format!("ghp_{}", "Z9".repeat(18));
    let aws = "AKIAABCDEFGHIJKLMNOP";
    std::fs::write(
        root.join("deploy.md"),
        format!("# Deploy\n\n## Token\n\nUse `{gh}` to push.\n"),
    )
    .unwrap();
    std::fs::write(
        root.join("notes.md"),
        format!("# Notes\n\nOld key {aws} rotated.\n"),
    )
    .unwrap();
    std::fs::write(
        root.join("secrets-migration.md"),
        "# Secrets migration\n\nWe moved every API key to SOPS + age.\n",
    )
    .unwrap();

    let mut engine = DocEngine::in_memory().unwrap();
    sync(&mut engine, root);

    // Trap files are withheld, prose about secrets is not.
    assert_eq!(chunks_of(&engine, "deploy.md"), 0);
    assert_eq!(chunks_of(&engine, "notes.md"), 0);
    assert!(chunks_of(&engine, "secrets-migration.md") > 0);
    let text = all_text(&engine);
    assert!(!text.contains(&gh) && !text.contains(aws));

    let issues = list(engine.conn()).unwrap();
    let quarantined: Vec<&str> = issues
        .iter()
        .filter(|i| i.kind == KIND_QUARANTINED)
        .map(|i| i.file_path.as_str())
        .collect();
    assert!(quarantined.contains(&"deploy.md") && quarantined.contains(&"notes.md"));
    // The reason names the kind of secret, never the secret.
    assert!(issues
        .iter()
        .all(|i| !i.reason.contains(&gh) && !i.reason.contains(aws)));

    // Once cleaned, the file comes back.
    std::fs::write(
        root.join("deploy.md"),
        "# Deploy\n\n## Token\n\nUse the vault token.\n",
    )
    .unwrap();
    sync(&mut engine, root);
    assert!(chunks_of(&engine, "deploy.md") > 0);
    let still: Vec<String> = list(engine.conn())
        .unwrap()
        .into_iter()
        .filter(|i| i.kind == KIND_QUARANTINED)
        .map(|i| i.file_path)
        .collect();
    assert_eq!(still, ["notes.md"]);
}

#[test]
fn a_file_that_gains_a_secret_loses_its_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("a.md"), "# A\n\nnothing to see\n").unwrap();
    let mut engine = DocEngine::in_memory().unwrap();
    sync(&mut engine, root);
    assert!(chunks_of(&engine, "a.md") > 0);
    std::fs::write(
        root.join("a.md"),
        "# A\n\n-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----\n",
    )
    .unwrap();
    sync(&mut engine, root);
    assert_eq!(chunks_of(&engine, "a.md"), 0);
    assert!(!all_text(&engine).contains("BEGIN RSA PRIVATE KEY"));
}
