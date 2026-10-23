//! Pure-ish backup helper: before any content-changing write to a config file, copy its
//! current bytes next to it. Base directory (`backup_dir`) and `timestamp` are always
//! injected by the caller — this module never resolves a cache directory or reads the clock
//! itself, keeping it fully testable on a tempdir.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// `<backup_dir>/<original_filename>.<timestamp>.bak`.
pub fn backup_path(backup_dir: &Path, original_filename: &str, timestamp: &str) -> PathBuf {
    backup_dir.join(format!("{original_filename}.{timestamp}.bak"))
}

/// Writes `new_content` to `config_path`, first backing up any existing content under
/// `backup_dir`. Returns the backup path, or `None` if `config_path` didn't exist yet (nothing
/// to back up). Creates parent directories as needed for both the config file and the backup.
pub fn write_with_backup(
    config_path: &Path,
    new_content: &str,
    backup_dir: &Path,
    timestamp: &str,
) -> io::Result<Option<PathBuf>> {
    let backup = if config_path.is_file() {
        let original = fs::read(config_path)?;
        fs::create_dir_all(backup_dir)?;
        let filename = config_path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("config");
        let path = backup_path(backup_dir, filename, timestamp);
        fs::write(&path, original)?;
        Some(path)
    } else {
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }
        None
    };
    fs::write(config_path, new_content)?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_path_format() {
        let p = backup_path(Path::new("/caches/backups"), "mcp.json", "20240101T000000Z");
        assert_eq!(
            p,
            Path::new("/caches/backups/mcp.json.20240101T000000Z.bak")
        );
    }

    #[test]
    fn writes_new_file_without_a_backup_when_none_existed() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("nested/mcp.json");
        let backups = dir.path().join("backups");
        let backup = write_with_backup(&config, "{}", &backups, "ts").unwrap();
        assert!(backup.is_none());
        assert_eq!(fs::read_to_string(&config).unwrap(), "{}");
        assert!(!backups.exists());
    }

    #[test]
    fn backs_up_previous_content_before_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("mcp.json");
        let backups = dir.path().join("backups");
        fs::write(&config, "{\"old\": true}").unwrap();
        let backup = write_with_backup(&config, "{\"new\": true}", &backups, "20240101T000000Z")
            .unwrap()
            .expect("a backup must be written");
        assert_eq!(fs::read_to_string(&backup).unwrap(), "{\"old\": true}");
        assert_eq!(fs::read_to_string(&config).unwrap(), "{\"new\": true}");
    }
}
