//! Per-document metadata: front matter of the file, plus agent annotations.
//!
//! The front matter schema used by `ingest` (`title`, `summary`, `tags`, `date`,
//! `source_type`, `category`). It makes results self-describing (an agent can
//! decide from the summary whether to open a file) and searchable by facet
//! (`tags`, `source_type`, `since`).
//!
//! Annotations are what an agent learned about a document (`annotate` MCP
//! tool): a summary and tags stored in the index, never in the source file, so
//! the next agent benefits without re-reading it. File metadata is rewritten
//! at every sync; annotations survive until the file disappears.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DocMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tags: Vec<String>,
    /// `YYYY-MM-DD` when declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// Declared type, else derived from the extension (`markdown`, `pdf`, `pptx`…).
    pub source_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// `file` (front matter) or `agent` (annotation merged in).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotated_by: Option<String>,
}

fn source_type_of(rel: &str) -> String {
    let ext = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "md" | "markdown" | "mdx" => "markdown".into(),
        "yml" => "yaml".into(),
        "" => "text".into(),
        other => other.into(),
    }
}

fn yaml_str(v: &serde_yaml::Value) -> Option<String> {
    match v {
        serde_yaml::Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Metadata of one file: its YAML front matter if any, else what the path says.
pub fn from_content(rel: &str, content: &str) -> DocMeta {
    let mut meta = DocMeta {
        source_type: source_type_of(rel),
        ..Default::default()
    };
    let body = content.strip_prefix('\u{feff}').unwrap_or(content);
    let Some(rest) = body
        .strip_prefix("---\n")
        .or_else(|| body.strip_prefix("---\r\n"))
    else {
        return meta;
    };
    let Some(end) = rest.find("\n---") else {
        return meta;
    };
    let Ok(serde_yaml::Value::Mapping(map)) =
        serde_yaml::from_str::<serde_yaml::Value>(&rest[..end])
    else {
        return meta;
    };
    let get = |k: &str| map.get(serde_yaml::Value::String(k.into()));
    meta.title = get("title").and_then(yaml_str);
    meta.summary = get("summary")
        .or_else(|| get("description"))
        .and_then(yaml_str);
    meta.category = get("category").and_then(yaml_str);
    meta.date = get("date")
        .and_then(yaml_str)
        .map(|d| d.chars().take(10).collect());
    if let Some(t) = get("source_type").and_then(yaml_str) {
        meta.source_type = t;
    }
    meta.tags = match get("tags") {
        Some(serde_yaml::Value::Sequence(seq)) => seq.iter().filter_map(yaml_str).collect(),
        Some(v) => yaml_str(v)
            .map(|s| {
                s.split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        None => Vec::new(),
    };
    meta
}

pub fn init_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS doc_meta (
            source_path TEXT PRIMARY KEY,
            meta TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS doc_annotations (
            source_path TEXT PRIMARY KEY,
            summary TEXT,
            tags TEXT NOT NULL,
            author TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        "#,
    )?;
    Ok(())
}

/// Replaces the file metadata of these paths (a sync rewrites them all).
pub fn upsert_many(conn: &mut Connection, entries: &[(&str, &DocMeta)]) -> Result<()> {
    init_tables(conn)?;
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO doc_meta (source_path, meta) VALUES (?1, ?2)",
        )?;
        for (path, meta) in entries {
            stmt.execute(params![path, serde_json::to_string(meta)?])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Paths that already have a metadata row.
pub fn known_paths(conn: &mut Connection) -> Result<std::collections::HashSet<String>> {
    init_tables(conn)?;
    let mut stmt = conn.prepare("SELECT source_path FROM doc_meta")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Drops metadata and annotations of files no longer in the index.
pub fn retain(conn: &mut Connection, active: &std::collections::HashSet<String>) -> Result<()> {
    init_tables(conn)?;
    for table in ["doc_meta", "doc_annotations"] {
        let paths: Vec<String> = {
            let mut stmt = conn.prepare(&format!("SELECT source_path FROM {table}"))?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let tx = conn.transaction()?;
        for p in paths.iter().filter(|p| !active.contains(*p)) {
            tx.execute(
                &format!("DELETE FROM {table} WHERE source_path = ?1"),
                params![p],
            )?;
        }
        tx.commit()?;
    }
    Ok(())
}

/// Records what an agent learned about a document. Tags are merged with the
/// existing annotation's; the summary replaces it when given.
pub fn annotate(
    conn: &Connection,
    source_path: &str,
    summary: Option<&str>,
    tags: &[String],
    author: &str,
) -> Result<()> {
    init_tables(conn)?;
    let indexed: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM doc_file_meta WHERE source_path = ?1)",
            params![source_path],
            |r| r.get(0),
        )
        .unwrap_or(false);
    if !indexed {
        anyhow::bail!(
            "'{source_path}' is not an indexed document (paths are relative to the workspace root)"
        );
    }
    let (old_summary, old_tags): (Option<String>, Vec<String>) = conn
        .query_row(
            "SELECT summary, tags FROM doc_annotations WHERE source_path = ?1",
            params![source_path],
            |r| Ok((r.get(0)?, r.get::<_, String>(1)?)),
        )
        .map(|(s, t)| (s, serde_json::from_str(&t).unwrap_or_default()))
        .unwrap_or_default();
    let mut all_tags = old_tags;
    for t in tags {
        let t = t.trim().to_lowercase();
        if !t.is_empty() && !all_tags.contains(&t) {
            all_tags.push(t);
        }
    }
    let summary = summary.map(str::to_string).or(old_summary);
    conn.execute(
        "INSERT OR REPLACE INTO doc_annotations (source_path, summary, tags, author, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            source_path,
            summary,
            serde_json::to_string(&all_tags)?,
            author,
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
        ],
    )?;
    Ok(())
}

/// Metadata of every document, annotations merged (annotation summary wins only
/// when the file declares none; tags are unioned).
pub fn load_all(conn: &Connection) -> Result<HashMap<String, DocMeta>> {
    init_tables(conn)?;
    let mut out: HashMap<String, DocMeta> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT source_path, meta FROM doc_meta")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (p, m) = row?;
            if let Ok(meta) = serde_json::from_str(&m) {
                out.insert(p, meta);
            }
        }
    }
    let mut stmt =
        conn.prepare("SELECT source_path, summary, tags, author FROM doc_annotations")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (p, summary, tags, author) = row?;
        let meta = out.entry(p.clone()).or_insert_with(|| DocMeta {
            source_type: source_type_of(&p),
            ..Default::default()
        });
        if meta.summary.is_none() {
            meta.summary = summary;
        }
        for t in serde_json::from_str::<Vec<String>>(&tags).unwrap_or_default() {
            if !meta.tags.iter().any(|x| x.eq_ignore_ascii_case(&t)) {
                meta.tags.push(t);
            }
        }
        meta.annotated_by = Some(author);
    }
    Ok(out)
}

/// Facets a search can be restricted to. Empty = no restriction.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchFilters {
    /// Every listed tag must be present (case-insensitive).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Any of these source types (`markdown`, `pdf`, `pptx`, `yaml`…).
    #[serde(default)]
    pub source_types: Vec<String>,
    /// Documents dated (front matter) or modified on/after this `YYYY-MM-DD`.
    #[serde(default)]
    pub since: Option<String>,
    /// Paths starting with one of these prefixes (relative to the root).
    #[serde(default)]
    pub path_prefixes: Vec<String>,
}

impl SearchFilters {
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
            && self.source_types.is_empty()
            && self.since.is_none()
            && self.path_prefixes.is_empty()
    }

    /// `modified` = file modification date (`YYYY-MM-DD`), used when the
    /// document declares no date.
    pub fn matches(&self, path: &str, meta: Option<&DocMeta>, modified: Option<&str>) -> bool {
        if !self.path_prefixes.is_empty()
            && !self
                .path_prefixes
                .iter()
                .any(|p| path.starts_with(p.as_str()))
        {
            return false;
        }
        let source_type = meta
            .map(|m| m.source_type.clone())
            .unwrap_or_else(|| source_type_of(path));
        if !self.source_types.is_empty()
            && !self
                .source_types
                .iter()
                .any(|t| t.eq_ignore_ascii_case(&source_type))
        {
            return false;
        }
        if !self.tags.is_empty() {
            let tags = meta.map(|m| m.tags.as_slice()).unwrap_or(&[]);
            if !self
                .tags
                .iter()
                .all(|t| tags.iter().any(|x| x.eq_ignore_ascii_case(t)))
            {
                return false;
            }
        }
        if let Some(since) = &self.since {
            let date = meta.and_then(|m| m.date.as_deref()).or(modified);
            if date.is_none_or(|d| d < since.as_str()) {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ingest_front_matter() {
        let md = "---\ntitle: \"Deck Kickoff\"\nsource_type: pptx\ndate: 2026-06-20\nsummary: Kickoff du projet.\ntags: [auth, pricing]\ncategory: specs\n---\n# Body\n";
        let m = from_content("context/sources/specs/kickoff.md", md);
        assert_eq!(m.title.as_deref(), Some("Deck Kickoff"));
        assert_eq!(m.source_type, "pptx");
        assert_eq!(m.date.as_deref(), Some("2026-06-20"));
        assert_eq!(m.tags, ["auth", "pricing"]);
        assert_eq!(from_content("a/b.pdf", "x").source_type, "pdf");
        assert!(from_content("a.md", "# no front matter").tags.is_empty());
    }

    #[test]
    fn filters_by_tags_type_date_and_path() {
        let m = DocMeta {
            tags: vec!["Auth".into(), "pricing".into()],
            date: Some("2026-06-20".into()),
            source_type: "pptx".into(),
            ..Default::default()
        };
        let f = |toml_src: &str| toml::from_str::<SearchFilters>(toml_src).unwrap();
        assert!(f("tags = [\"auth\"]").matches("x.md", Some(&m), None));
        assert!(!f("tags = [\"auth\", \"legal\"]").matches("x.md", Some(&m), None));
        assert!(f("source_types = [\"pptx\", \"pdf\"]").matches("x.md", Some(&m), None));
        assert!(!f("since = \"2026-07-01\"").matches("x.md", Some(&m), None));
        assert!(f("since = \"2026-07-01\"").matches("y.md", None, Some("2026-08-01")));
        assert!(!f("path_prefixes = [\"docs/\"]").matches("src/x.md", Some(&m), None));
    }

    #[test]
    fn annotations_merge_and_follow_the_file() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE doc_file_meta (source_path TEXT PRIMARY KEY, mtime_millis INTEGER, content_hash INTEGER);
             INSERT INTO doc_file_meta VALUES ('a.md', 0, 0);",
        )
        .unwrap();
        assert!(annotate(&conn, "missing.md", Some("x"), &[], "agent").is_err());
        let file = DocMeta {
            tags: vec!["infra".into()],
            source_type: "markdown".into(),
            ..Default::default()
        };
        upsert_many(&mut conn, &[("a.md", &file)]).unwrap();
        annotate(
            &conn,
            "a.md",
            Some("Explique le tunnel."),
            &["Cloudflare".into()],
            "claude",
        )
        .unwrap();
        annotate(&conn, "a.md", None, &["dns".into()], "claude").unwrap();
        let all = load_all(&conn).unwrap();
        let a = &all["a.md"];
        assert_eq!(a.summary.as_deref(), Some("Explique le tunnel."));
        assert_eq!(a.tags, ["infra", "cloudflare", "dns"]);
        assert_eq!(a.annotated_by.as_deref(), Some("claude"));
        retain(&mut conn, &Default::default()).unwrap();
        assert!(load_all(&conn).unwrap().is_empty());
    }
}
