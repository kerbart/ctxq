//! Shared SQLite open: WAL + a long busy timeout so the daemon worker, the
//! refresher, MCP and the CLI can share `docs.db` without `SQLITE_BUSY` on
//! the second query.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, Error, ErrorCode};

/// How long a connection waits for a lock before returning `SQLITE_BUSY`.
/// A code-graph sync of a large tree holds write transactions well past 5 s.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(60);

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    configure(&conn)?;
    Ok(conn)
}

pub fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    // WAL: readers proceed while a writer is in a transaction.
    let _ = conn.pragma_update(None, "journal_mode", "WAL");
    let _ = conn.pragma_update(None, "synchronous", "NORMAL");
    let _ = conn.pragma_update(None, "temp_store", "MEMORY");
    let _ = conn.pragma_update(None, "foreign_keys", "ON");
    Ok(())
}

pub fn is_busy(err: &Error) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Instant;

    #[test]
    fn open_enables_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let conn = open(&path).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "wal");
    }

    #[test]
    fn second_writer_waits_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let a = open(&path).unwrap();
        a.execute_batch("CREATE TABLE t(x INTEGER);").unwrap();
        a.execute("BEGIN IMMEDIATE", []).unwrap();
        a.execute("INSERT INTO t VALUES (1)", []).unwrap();

        let path_b = path.clone();
        let started = Instant::now();
        let handle = thread::spawn(move || {
            let b = open(&path_b).unwrap();
            b.execute("INSERT INTO t VALUES (2)", []).unwrap();
        });
        thread::sleep(Duration::from_millis(200));
        a.execute("COMMIT", []).unwrap();
        handle
            .join()
            .expect("second writer should succeed after wait");
        assert!(
            started.elapsed() >= Duration::from_millis(200),
            "second writer must have waited on the lock"
        );
        let n: i64 = a
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
    }
}
