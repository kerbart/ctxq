//! Indexing issues (quarantine, unusable file).
//! Feeds `doctor` / `status` and the secrets tests.

use std::collections::HashSet;

use anyhow::Result;
use rusqlite::{params, Connection};

pub const KIND_QUARANTINED: &str = "quarantined";
pub const KIND_UNUSABLE: &str = "unusable";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseIssue {
    pub file_path: String,
    pub kind: String,
    pub reason: String,
    pub severity: String,
}

pub fn init_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS parse_issues (
            file_path TEXT NOT NULL,
            kind TEXT NOT NULL,
            reason TEXT NOT NULL,
            severity TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_parse_issues_file ON parse_issues(file_path);",
    )?;
    Ok(())
}

pub fn list(conn: &Connection) -> Result<Vec<ParseIssue>> {
    init_table(conn)?;
    let mut stmt = conn.prepare(
        "SELECT file_path, kind, reason, severity FROM parse_issues ORDER BY file_path, kind, reason",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ParseIssue {
            file_path: r.get(0)?,
            kind: r.get(1)?,
            reason: r.get(2)?,
            severity: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Replaces a file's issues (an empty list clears them).
pub fn set_for_file(conn: &Connection, file: &str, issues: &[ParseIssue]) -> Result<()> {
    init_table(conn)?;
    conn.execute(
        "DELETE FROM parse_issues WHERE file_path = ?1",
        params![file],
    )?;
    for issue in issues {
        conn.execute(
            "INSERT INTO parse_issues (file_path, kind, reason, severity) VALUES (?1, ?2, ?3, ?4)",
            params![issue.file_path, issue.kind, issue.reason, issue.severity],
        )?;
    }
    Ok(())
}

pub fn retain(conn: &Connection, active: &HashSet<String>) -> Result<()> {
    init_table(conn)?;
    let paths: Vec<String> = {
        let mut stmt = conn.prepare("SELECT DISTINCT file_path FROM parse_issues")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for p in paths.iter().filter(|p| !active.contains(*p)) {
        conn.execute("DELETE FROM parse_issues WHERE file_path = ?1", params![p])?;
    }
    Ok(())
}
