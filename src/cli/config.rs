//! Index location for a workspace.
//!
//! `./.documentum/` if it exists, otherwise `$DOCUMENTUM_CACHE_DIR` or
//! `~/.cache/documentum/<name>-<hash>/`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct ProjectPaths {
    pub root_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub docs_db: PathBuf,
}

impl ProjectPaths {
    pub fn discover(target: Option<&Path>) -> Result<Self> {
        let root_dir = match target {
            Some(p) => absolutize(p),
            None => std::env::current_dir().context("current directory")?,
        };
        let cache_dir = if root_dir.join(".documentum").is_dir() {
            root_dir.join(".documentum")
        } else {
            cache_base()?.join(cache_key(&root_dir))
        };
        std::fs::create_dir_all(&cache_dir)
            .with_context(|| format!("creating {}", cache_dir.display()))?;
        let docs_db = cache_dir.join("docs.db");
        Ok(Self {
            root_dir,
            cache_dir,
            docs_db,
        })
    }
}

fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    } else {
        let joined = std::env::current_dir().unwrap_or_default().join(p);
        std::fs::canonicalize(&joined).unwrap_or(joined)
    }
}

fn cache_base() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("DOCUMENTUM_CACHE_DIR") {
        return Ok(PathBuf::from(p));
    }
    if let Some(p) = std::env::var_os("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(p).join("documentum"));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .context("HOME is not set (define DOCUMENTUM_CACHE_DIR)")?;
    Ok(home.join(".cache").join("documentum"))
}

fn cache_key(root: &Path) -> String {
    let name = root
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("root");
    let mut hasher = Sha256::new();
    hasher.update(root.to_string_lossy().as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    format!("{name}-{}", &hex[..16])
}
