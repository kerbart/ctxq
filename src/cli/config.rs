//! Index location for a workspace.
//!
//! `./.ctxq/` if it exists, otherwise `$CTXQ_CACHE_DIR` or
//! `~/.cache/ctxq/<name>-<hash>/`.

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
        let cache_dir = if root_dir.join(".ctxq").is_dir() {
            root_dir.join(".ctxq")
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
    if let Some(p) = std::env::var_os("CTXQ_CACHE_DIR") {
        return Ok(PathBuf::from(p));
    }
    if let Some(p) = std::env::var_os("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(p).join("ctxq"));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .context("HOME is not set (define CTXQ_CACHE_DIR)")?;
    Ok(home.join(".cache").join("ctxq"))
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

fn same_path(a: &Path, b: &Path) -> bool {
    let ca = std::fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let cb = std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    ca == cb
}

fn candidate_homes() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(h) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    {
        out.push(h);
    }
    if let Some(user) = std::env::var_os("USER").or_else(|| std::env::var_os("LOGNAME")) {
        if !user.is_empty() {
            out.push(PathBuf::from("/home").join(&user));
            out.push(PathBuf::from("/Users").join(&user));
        }
    }
    out
}

fn display_homeish(p: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    {
        if let Ok(rest) = p.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~".into();
            }
            return format!("~/{}", rest.display());
        }
    }
    p.display().to_string()
}

pub fn is_home_root(root: &Path) -> bool {
    candidate_homes().iter().any(|h| same_path(root, h))
}

/// Where the index lives, what it covers, and that each workspace root is independent.
pub fn describe_index(paths: &ProjectPaths) -> String {
    let mut lines = vec![
        format!("   workspace  {}", display_homeish(&paths.root_dir)),
        format!("   index      {}", display_homeish(&paths.docs_db)),
        "   covers     markdown, office, PDF, code  ·  skips .git, archives, secrets, gitignored files"
            .into(),
    ];
    if is_home_root(&paths.root_dir) {
        lines.push(
            "   note       this is your whole home. A project (`cd ~/engie && ctxq setup`) gets its own index — two indexes, they do not merge."
                .into(),
        );
    } else {
        lines.push(
            "   note       one index per workspace root. Indexing $HOME and then a folder inside it makes two indexes."
                .into(),
        );
    }
    lines.join("\n")
}

/// Indexing `$HOME` (or `/home/$USER`) walks the whole machine. Refuse unless asked.
pub fn refuse_home(root: &Path, force_home: bool) -> Result<()> {
    if force_home {
        return Ok(());
    }
    for home in candidate_homes() {
        if same_path(root, &home) {
            anyhow::bail!(
                "refusing to index {}: run `ctxq setup` from a project directory (or pass --force-home)",
                home.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod home_guard_tests {
    use super::refuse_home;
    use std::path::PathBuf;

    #[test]
    fn force_home_allows_anything() {
        refuse_home(&PathBuf::from("/"), true).unwrap();
    }

    #[test]
    fn home_is_refused() {
        let home = std::env::var_os("HOME").expect("HOME");
        let err = refuse_home(&PathBuf::from(home), false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("refusing to index"), "{msg}");
        assert!(msg.contains("--force-home"), "{msg}");
    }

    #[test]
    fn login_home_is_refused_even_if_env_home_differs() {
        let user = std::env::var_os("USER").or_else(|| std::env::var_os("LOGNAME"));
        let Some(user) = user else { return };
        for base in ["/home", "/Users"] {
            let p = PathBuf::from(base).join(&user);
            if p.is_dir() {
                let err = refuse_home(&p, false).unwrap_err();
                assert!(err.to_string().contains("refusing to index"), "{err}");
                return;
            }
        }
    }

    #[test]
    fn a_project_dir_is_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        refuse_home(tmp.path(), false).unwrap();
    }

    #[test]
    fn home_and_subdir_get_distinct_cache_keys() {
        let a = super::cache_key(std::path::Path::new("/home/damien"));
        let b = super::cache_key(std::path::Path::new("/home/damien/engie"));
        assert_ne!(a, b);
        assert!(a.starts_with("damien-"), "{a}");
        assert!(b.starts_with("engie-"), "{b}");
    }

    #[test]
    fn describe_index_says_two_indexes() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = super::ProjectPaths::discover(Some(tmp.path())).unwrap();
        let text = super::describe_index(&paths);
        assert!(text.contains("workspace"), "{text}");
        assert!(text.contains("docs.db"), "{text}");
        assert!(text.contains("two indexes"), "{text}");
        assert!(text.contains("markdown"), "{text}");
    }
}
