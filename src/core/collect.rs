//! Document discovery filters, driven by `[collect]`.
//!
//! Filtering is not cosmetic: dependencies, archives and the documentation of
//! cloned third-party repositories drown a corpus. On ctxq's test base,
//! dropping them halved the corpus with no relevance loss, halving memory,
//! rebuild time and embedding cost. No directory name of any particular
//! machine is assumed here: every rule comes from the settings.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;

use crate::core::settings::CollectSettings;

static REMOTE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*url\s*=\s*(\S+)").unwrap());

/// Reads a glob file (one pattern per line, `#` comments, relative to the root).
pub fn read_globs(path: &Path) -> Result<GlobSet> {
    exclusion_globs(Some(path), &[])
}

/// Exclusion globs of a file plus inline ones; `dir` also excludes everything below it.
pub fn exclusion_globs(file: Option<&Path>, inline: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    let text = file
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let lines = text
        .lines()
        .map(str::to_string)
        .chain(inline.iter().cloned());
    for line in lines {
        let pat = line
            .split('#')
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches('/')
            .to_string();
        if pat.is_empty() {
            continue;
        }
        builder.add(Glob::new(&pat).with_context(|| format!("exclusion glob '{pat}'"))?);
        builder.add(Glob::new(&format!("{pat}/**"))?);
    }
    Ok(builder.build()?)
}

pub fn globset(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        builder.add(Glob::new(p.trim_matches('/')).with_context(|| format!("glob '{p}'"))?);
    }
    Ok(builder.build()?)
}

#[derive(Debug, Clone)]
pub struct DiscoveryFilter {
    root: PathBuf,
    settings: CollectSettings,
    excludes: GlobSet,
}

impl DiscoveryFilter {
    pub fn new(root: &Path, settings: &CollectSettings) -> Result<Self> {
        let file = settings.exclude_globs_file.as_ref().map(|f| root.join(f));
        let excludes = exclusion_globs(file.as_deref(), &settings.exclude_globs)?;
        Ok(Self {
            root: root.to_path_buf(),
            settings: settings.clone(),
            excludes,
        })
    }

    /// Directories to walk, in priority order (the root itself by default).
    pub fn roots(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = self
            .settings
            .roots
            .iter()
            .map(|r| self.root.join(r))
            .filter(|p| p.is_dir())
            .collect();
        if out.is_empty() {
            out.push(self.root.clone());
        }
        out
    }

    pub fn respect_gitignore(&self) -> bool {
        self.settings.respect_gitignore
    }

    fn excluded(&self, path: &Path) -> bool {
        !self.excludes.is_empty()
            && path
                .strip_prefix(&self.root)
                .map(|rel| self.excludes.is_match(rel))
                .unwrap_or(false)
    }

    /// A sub-directory with its own repository whose remote matches none of
    /// `own_remotes`: third-party code, not own knowledge. A `.git` *file*
    /// (worktree, submodule) cannot decide: kept.
    fn is_foreign_clone(&self, dir: &Path) -> bool {
        // A top-level directory of the root is a project, not a clone inside one.
        if self.settings.own_remotes.is_empty()
            || dir == self.root
            || dir.parent() == Some(self.root.as_path())
        {
            return false;
        }
        let Ok(cfg) = std::fs::read_to_string(dir.join(".git").join("config")) else {
            return false;
        };
        let urls: Vec<&str> = REMOTE_RE
            .captures_iter(&cfg)
            .filter_map(|c| c.get(1).map(|m| m.as_str()))
            .collect();
        !urls.is_empty()
            && !urls.iter().any(|u| {
                self.settings
                    .own_remotes
                    .iter()
                    .any(|o| u.contains(o.as_str()))
            })
    }

    pub fn skip_dir(&self, path: &Path) -> bool {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // WalkBuilder is run with hidden(false) so `.github` is indexed; `.git`
        // is not a document source (objects, hooks, index). Same for other VCS.
        if matches!(name, ".git" | ".svn" | ".hg") {
            return true;
        }
        if path.parent() == Some(self.root.as_path())
            && self.settings.exclude_top.iter().any(|t| t == name)
        {
            return true;
        }
        self.settings.exclude_dirs.iter().any(|d| d == name)
            || self
                .settings
                .exclude_dir_names
                .iter()
                .any(|d| d.eq_ignore_ascii_case(name))
            || self.excluded(path)
            || self.is_foreign_clone(path)
    }

    pub fn skip_file(&self, path: &Path, len: u64) -> bool {
        // ctxq's own configuration is tooling, not knowledge.
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if matches!(name, "ctxq.toml" | ".ctxq.toml" | ".ctxqignore") {
            return true;
        }
        let document = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(crate::core::convert::is_convertible);
        let limit = if document {
            self.settings.max_document_bytes
        } else {
            self.settings.max_file_bytes
        };
        if limit > 0 && len > limit {
            return true;
        }
        if !self.settings.extensions.is_empty() {
            let ok = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                self.settings
                    .extensions
                    .iter()
                    .any(|x| x.eq_ignore_ascii_case(e))
            });
            if !ok {
                return true;
            }
        }
        self.excluded(path)
    }

    /// Rank of the root a relative path belongs to (lower = higher priority).
    pub fn root_rank(&self, rel: &str) -> usize {
        rank_in_roots(&self.settings.roots, rel)
    }
}

pub fn rank_in_roots(roots: &[String], rel: &str) -> usize {
    roots
        .iter()
        .position(|r| {
            let r = r.trim_start_matches("./").trim_end_matches('/');
            r.is_empty() || r == "." || rel == r || rel.starts_with(&format!("{r}/"))
        })
        .unwrap_or(roots.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_globs_names_and_foreign_clones() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join(".ctxqignore"),
            "vendored\nnotes/private.md # secret\n",
        )
        .unwrap();
        for d in [
            "vendored",
            "Archive",
            "mine/.git",
            "theirs/.git",
            "proj/theirs/.git",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(
            root.join("mine/.git/config"),
            "[remote \"origin\"]\n  url = git@github.com:me/mine\n",
        )
        .unwrap();
        std::fs::write(
            root.join("theirs/.git/config"),
            "[remote \"origin\"]\n  url = https://github.com/else/x\n",
        )
        .unwrap();
        std::fs::write(
            root.join("proj/theirs/.git/config"),
            "[remote \"origin\"]\n  url = https://github.com/else/x\n",
        )
        .unwrap();
        let mut s = CollectSettings {
            own_remotes: vec!["github.com:me/".into()],
            ..Default::default()
        };
        let f = DiscoveryFilter::new(root, &s).unwrap();
        assert!(f.skip_dir(&root.join("vendored")));
        assert!(f.skip_dir(&root.join("Archive")));
        std::fs::create_dir_all(root.join(".git")).unwrap();
        assert!(f.skip_dir(&root.join(".git")));
        assert!(f.skip_dir(&root.join("mine/.git")));
        assert!(!f.skip_dir(&root.join("mine")));
        // Top level = a project of its own; nested foreign repository = a clone.
        assert!(!f.skip_dir(&root.join("theirs")));
        assert!(f.skip_dir(&root.join("proj/theirs")));
        assert!(f.skip_file(&root.join("notes/private.md"), 10));
        assert!(!f.skip_file(&root.join("notes/public.md"), 10));
        s.extensions = vec!["md".into()];
        s.max_file_bytes = 100;
        let f = DiscoveryFilter::new(root, &s).unwrap();
        assert!(f.skip_file(&root.join("a.yaml"), 10));
        assert!(f.skip_file(&root.join("a.md"), 101));
    }

    #[test]
    fn ranks_by_declared_roots() {
        let roots = vec!["docs".to_string(), ".".to_string()];
        assert_eq!(rank_in_roots(&roots, "docs/a.md"), 0);
        assert_eq!(rank_in_roots(&roots, "proj/a.md"), 1);
    }
}
