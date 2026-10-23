//! Markdown chunking for documentum.
//!
//! One chunk = one section (level 2 or 3 heading), prefixed with its
//! breadcrumb; the text stays raw markdown. Large tables in the generated
//! inventories are additionally indexed row by row: without that, a pointed
//! question about one service drowns in the whole table. This chunking is the
//! one that holds MRR 0.79 on the golden set; the one in
//! `parser::parse_markdown` (one section per heading, re-rendered text) tops
//! out at 0.57.

use std::sync::LazyLock;

use regex::Regex;

use crate::core::doctext::split_lines;
use crate::core::parser::{extract_symbol_references, generate_chunk_id, DocChunk};

static HEADING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(#{1,6})\s+(.*?)\s*#*$").unwrap());
static TABLE_ROW_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\|.*\|\s*$").unwrap());
static TABLE_SEP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\|[\s:|-]+\|\s*$").unwrap());

/// Below this, a table stays a single chunk: it is short enough for that.
const MIN_TABLE_ROWS: usize = 4;

/// Breadcrumb separator, shared with the lexical side.
pub const CRUMB_SEP: &str = " > ";

struct Section {
    crumbs: Vec<String>,
    title: String,
    level: u32,
    line_start: usize,
    line_end: usize,
    text: String,
}

/// True if `rel_path` is under one of the directories (relative to the root).
///
/// Tables are split row by row only in declared directories
/// (`[collect].table_row_dirs` + inventory output): elsewhere, an isolated
/// table row loses its context.
pub fn in_dirs(rel_path: &str, dirs: &[String]) -> bool {
    dirs.iter().any(|d| rel_path.starts_with(&format!("{d}/")))
}

fn doc_title_for(rel_path: &str, lines: &[&str]) -> String {
    for line in lines {
        if let Some(c) = HEADING_RE.captures(line) {
            if c[1].len() == 1 {
                return c[2].trim().to_string();
            }
        }
    }
    std::path::Path::new(rel_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

/// Splits at level 2 and 3 headings; the preamble becomes a "doc" chunk.
fn split_sections(lines: &[&str], doc_title: &str) -> Vec<Section> {
    let boundaries: Vec<(usize, usize, String)> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| {
            let c = HEADING_RE.captures(line)?;
            let level = c[1].len();
            (level == 2 || level == 3).then(|| (i, level, c[2].trim().to_string()))
        })
        .collect();

    let mut out = Vec::new();
    let first = boundaries.first().map(|b| b.0).unwrap_or(lines.len());
    let intro = lines[..first].join("\n").trim().to_string();
    if !intro.is_empty() {
        out.push(Section {
            crumbs: vec![doc_title.to_string()],
            title: doc_title.to_string(),
            level: 1,
            line_start: 1,
            line_end: first.max(1),
            text: intro,
        });
    }

    let mut last_h2: Option<String> = None;
    for (idx, (start, level, title)) in boundaries.iter().enumerate() {
        let end = boundaries.get(idx + 1).map(|b| b.0).unwrap_or(lines.len());
        let text = lines[*start..end].join("\n").trim().to_string();
        if text.is_empty() {
            continue;
        }
        let crumbs = if *level == 2 {
            last_h2 = Some(title.clone());
            vec![doc_title.to_string(), title.clone()]
        } else {
            match &last_h2 {
                Some(h2) => vec![doc_title.to_string(), h2.clone(), title.clone()],
                None => vec![doc_title.to_string(), title.clone()],
            }
        };
        out.push(Section {
            crumbs,
            title: title.clone(),
            level: *level as u32,
            line_start: start + 1,
            line_end: end,
            text,
        });
    }
    out
}

/// One chunk per data row of large tables (header + separator + row), so
/// that each service stays individually findable.
fn table_row_chunks(sec: &Section) -> Vec<Section> {
    let lines: Vec<&str> = split_lines(&sec.text);
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let is_table_head = TABLE_ROW_RE.is_match(lines[i])
            && i + 1 < lines.len()
            && TABLE_SEP_RE.is_match(lines[i + 1]);
        if !is_table_head {
            i += 1;
            continue;
        }
        let (header, sep) = (lines[i], lines[i + 1]);
        let mut rows = Vec::new();
        let mut j = i + 2;
        while j < lines.len() && TABLE_ROW_RE.is_match(lines[j]) {
            rows.push(j);
            j += 1;
        }
        if rows.len() >= MIN_TABLE_ROWS {
            for &r in &rows {
                let cell = lines[r]
                    .trim()
                    .trim_matches('|')
                    .split('|')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .replace('`', "");
                let cell = if cell.is_empty() {
                    format!("ligne {r}")
                } else {
                    cell
                };
                // The reference breadcrumb is "crumb | cell": the cell is
                // glued to the last link, not added as a new link.
                let mut crumbs = sec.crumbs.clone();
                if let Some(last) = crumbs.last_mut() {
                    *last = format!("{last} | {cell}");
                }
                out.push(Section {
                    crumbs,
                    title: cell,
                    level: sec.level,
                    line_start: sec.line_start + r,
                    line_end: sec.line_start + r,
                    text: format!("{header}\n{sep}\n{}", lines[r]),
                });
            }
        }
        i = j;
    }
    out
}

/// Splits a markdown file into documentum chunks.
pub fn chunk_markdown(rel_path: &str, content: &str, per_row: bool) -> Vec<DocChunk> {
    let lines: Vec<&str> = split_lines(content);
    let title = doc_title_for(rel_path, &lines);
    let mut out = Vec::new();
    for sec in split_sections(&lines, &title) {
        // The section first, then its table rows (reference order).
        let rows = if per_row {
            table_row_chunks(&sec)
        } else {
            Vec::new()
        };
        for s in std::iter::once(sec).chain(rows) {
            let idx = out.len();
            out.push(DocChunk {
                id: generate_chunk_id(rel_path, &s.title, idx),
                source_path: rel_path.to_string(),
                symbol_references: extract_symbol_references(&s.text),
                section_title: s.title,
                heading_level: s.level,
                hierarchy: s.crumbs,
                content: s.text,
                start_line: s.line_start,
                end_line: s.line_end,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_at_h2_h3_with_breadcrumbs() {
        let md = "# Doc\nintro\n## A\na\n### B\nb\n## C\nc\n";
        let ch = chunk_markdown("x/doc.md", md, false);
        let crumbs: Vec<String> = ch.iter().map(|c| c.hierarchy.join(CRUMB_SEP)).collect();
        assert_eq!(crumbs, ["Doc", "Doc > A", "Doc > A > B", "Doc > C"]);
        assert_eq!(ch[2].start_line, 5);
        assert!(ch[1].content.starts_with("## A"));
    }

    #[test]
    fn table_rows_only_in_declared_dirs() {
        let md = "# Inv\n## Ports\n| port | svc |\n|---|---|\n| 3003 | alpha |\n| 3004 | todo |\n| 3005 | sami |\n| 3006 | gen |\n";
        let dirs = vec!["docs/generated".to_string()];
        assert!(!in_dirs("notes/x.md", &dirs));
        assert!(in_dirs("docs/generated/ports.md", &dirs));
        assert_eq!(chunk_markdown("notes/x.md", md, false).len(), 2);
        let ch = chunk_markdown("docs/generated/ports.md", md, true);
        assert_eq!(ch.len(), 6);
        assert_eq!(ch[2].hierarchy.join(CRUMB_SEP), "Inv > Ports | 3003");
        assert!(ch[2].content.ends_with("| 3003 | alpha |"));
    }
}
