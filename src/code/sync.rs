//! Walks a tree, feeds changed files through `extract`, keeps the store in sync.
//! See docs/v3/CODEGRAPH.md ("Storage").

use std::collections::HashSet;
use std::path::Path;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::model::{Lang, EXTRACTOR_VERSION};
use super::store::CodeStore;

/// Ignore behaviour for a sync pass. `.gitignore` is always a baseline rule of the `ignore`
/// crate; `custom_ignore_files` adds further gitignore-syntax files (`.ctxqignore` by
/// default) honoured in every directory of the walk.
#[derive(Debug, Clone)]
pub struct IgnoreRules {
    pub respect_gitignore: bool,
    pub custom_ignore_files: Vec<String>,
    /// Files larger than this are skipped entirely (never extracted, never stored).
    pub max_file_bytes: u64,
}

impl Default for IgnoreRules {
    fn default() -> Self {
        Self {
            respect_gitignore: true,
            custom_ignore_files: vec![".ctxqignore".to_string()],
            max_file_bytes: 2 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncReport {
    pub scanned: usize,
    pub extracted: usize,
    pub skipped_unchanged: usize,
    pub skipped_too_large: usize,
    pub removed: usize,
    /// Set when `EXTRACTOR_VERSION` differed from the version stamped in the store: every
    /// file was re-extracted regardless of mtime/size/hash.
    pub forced_reextraction: bool,
}

/// Walks `root`, (re-)extracts changed files into `store`, drops files that disappeared, then
/// resolves calls. Unknown extensions (per `Lang::for_path`) are not visited at all.
pub fn sync_tree(store: &mut CodeStore, root: &Path, rules: &IgnoreRules) -> Result<SyncReport> {
    sync_tree_with(store, root, rules, None)
}

/// Like [`sync_tree`], with a tick after each visited source file.
pub fn sync_tree_with(
    store: &mut CodeStore,
    root: &Path,
    rules: &IgnoreRules,
    mut on_file: Option<&mut dyn FnMut(usize, &str)>,
) -> Result<SyncReport> {
    let stored_version = store.extractor_version()?;
    let force = stored_version != Some(EXTRACTOR_VERSION);

    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .git_ignore(rules.respect_gitignore)
        .git_global(rules.respect_gitignore)
        .git_exclude(rules.respect_gitignore)
        .require_git(false)
        .parents(true);
    for name in &rules.custom_ignore_files {
        builder.add_custom_ignore_filename(name);
    }

    let mut report = SyncReport {
        forced_reextraction: force,
        ..Default::default()
    };
    let mut seen: HashSet<String> = HashSet::new();

    for entry in builder.build() {
        let entry = entry?;
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let Some(lang) = Lang::for_path(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        report.scanned += 1;
        if let Some(cb) = on_file.as_mut() {
            cb(report.scanned, &rel);
        }

        let metadata = entry.metadata()?;
        let size = metadata.len();
        if rules.max_file_bytes > 0 && size > rules.max_file_bytes {
            report.skipped_too_large += 1;
            continue;
        }
        seen.insert(rel.clone());

        let mtime = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let existing = store.file_meta(&rel)?;

        if !force {
            if let Some(meta) = &existing {
                if meta.mtime == mtime && meta.size as u64 == size {
                    report.skipped_unchanged += 1;
                    continue;
                }
            }
        }

        let bytes = std::fs::read(path)?;
        let sha256 = format!("{:x}", Sha256::digest(&bytes));

        if !force {
            if let Some(meta) = &existing {
                if meta.sha256 == sha256 {
                    // mtime moved (e.g. a checkout) but content is identical: skip re-extraction.
                    store.touch_file(&rel, mtime, size as i64)?;
                    report.skipped_unchanged += 1;
                    continue;
                }
            }
        }

        let source = String::from_utf8_lossy(&bytes);
        let outline = crate::code::extract::extract(lang, &source);
        store.upsert_file(&rel, lang, mtime, size as i64, &sha256, &outline)?;
        report.extracted += 1;
    }

    for existing_path in store.all_file_paths()? {
        if !seen.contains(&existing_path) {
            store.remove_file(&existing_path)?;
            report.removed += 1;
        }
    }

    store.set_extractor_version(EXTRACTOR_VERSION)?;
    store.resolve_calls()?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::model::{FileOutline, Symbol, SymbolKind};
    use std::fs;

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, content).unwrap();
    }

    #[test]
    fn syncs_new_files_and_skips_unchanged_on_second_pass() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "a.rs", "fn main() {}\n");

        let mut store = CodeStore::in_memory().unwrap();
        let report1 = sync_tree(&mut store, root, &IgnoreRules::default()).unwrap();
        assert_eq!(report1.extracted, 1);
        assert_eq!(report1.scanned, 1);

        let report2 = sync_tree(&mut store, root, &IgnoreRules::default()).unwrap();
        assert_eq!(report2.extracted, 0);
        assert_eq!(report2.skipped_unchanged, 1);
    }

    #[test]
    fn removes_deleted_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "a.rs", "fn main() {}\n");

        let mut store = CodeStore::in_memory().unwrap();
        sync_tree(&mut store, root, &IgnoreRules::default()).unwrap();
        assert_eq!(store.all_file_paths().unwrap(), vec!["a.rs".to_string()]);

        fs::remove_file(root.join("a.rs")).unwrap();
        let report = sync_tree(&mut store, root, &IgnoreRules::default()).unwrap();
        assert_eq!(report.removed, 1);
        assert!(store.all_file_paths().unwrap().is_empty());
    }

    #[test]
    fn respects_gitignore_and_ctxqignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, ".gitignore", "ignored_by_git.rs\n");
        write(root, ".ctxqignore", "ignored_by_custom.rs\n");
        write(root, "ignored_by_git.rs", "fn a() {}\n");
        write(root, "ignored_by_custom.rs", "fn b() {}\n");
        write(root, "kept.rs", "fn c() {}\n");

        let mut store = CodeStore::in_memory().unwrap();
        sync_tree(&mut store, root, &IgnoreRules::default()).unwrap();
        let mut paths = store.all_file_paths().unwrap();
        paths.sort();
        assert_eq!(paths, vec!["kept.rs".to_string()]);
    }

    #[test]
    fn skips_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "big.rs", &"x".repeat(100));

        let mut store = CodeStore::in_memory().unwrap();
        let rules = IgnoreRules {
            max_file_bytes: 10,
            ..Default::default()
        };
        let report = sync_tree(&mut store, root, &rules).unwrap();
        assert_eq!(report.skipped_too_large, 1);
        assert!(store.all_file_paths().unwrap().is_empty());
    }

    #[test]
    fn extractor_version_bump_forces_full_reextraction() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "a.rs", "fn main() {}\n");

        let mut store = CodeStore::in_memory().unwrap();
        sync_tree(&mut store, root, &IgnoreRules::default()).unwrap();
        // Simulate a prior run stamped with an older extractor version: the real extractor is
        // a stub today (EXTRACTOR_VERSION never actually changes within this test), so we
        // directly manipulate the stored stamp to exercise the "force" branch, and confirm
        // the file is re-extracted (not skipped as unchanged) even though mtime/size/hash
        // haven't moved.
        store
            .set_extractor_version(EXTRACTOR_VERSION.wrapping_add(1))
            .unwrap();
        // Hand-insert a symbol so we can tell apart "kept" vs "replaced" content below.
        let outline = FileOutline {
            symbols: vec![Symbol {
                name: "stale".to_string(),
                qname: "stale".to_string(),
                kind: SymbolKind::Function,
                lang: Lang::Rust,
                start_line: 1,
                end_line: 1,
                signature: "fn stale()".to_string(),
                doc: String::new(),
                parent_qname: None,
            }],
            calls: vec![],
            imports: vec![],
        };
        store
            .upsert_file("a.rs", Lang::Rust, 1, 1, "stale-hash", &outline)
            .unwrap();

        let report = sync_tree(&mut store, root, &IgnoreRules::default()).unwrap();
        assert!(report.forced_reextraction);
        assert_eq!(report.extracted, 1);
        let still_stale: i64 = store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM code_symbols WHERE qname = 'stale'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            still_stale, 0,
            "stale symbol should have been replaced by re-extraction"
        );
    }
}
