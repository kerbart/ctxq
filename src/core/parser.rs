//! Generic markdown chunker and shared contracts (identifiers, symbols).
//!
//! The chunking that carries the relevance of `.md` documents is
//! [`crate::core::mdchunk`]. This one is a fallback for plain text.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

use crate::core::vector::hash_str;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocChunk {
    pub id: String,
    pub source_path: String,
    pub section_title: String,
    pub heading_level: u32,
    pub hierarchy: Vec<String>,
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol_references: Vec<String>,
}

/// `@kind: Name` -> the part after the colon, otherwise the trimmed name.
pub fn symbol_ref(name: &str) -> String {
    let name = name.trim();
    let Some(rest) = name.strip_prefix('@') else {
        return name.to_string();
    };
    let Some((kind, value)) = rest.split_once(':') else {
        return name.to_string();
    };
    if !kind.is_empty() && kind.chars().all(|c| c.is_ascii_alphanumeric()) {
        value.trim().to_string()
    } else {
        name.to_string()
    }
}

static AT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"@(service|symbol|fn|func|interface|class|struct|module):\s*([A-Za-z0-9_.:\-]+)")
        .expect("at-ref")
});
static TICK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"`([A-Za-z_][A-Za-z0-9_\-]*(?:(?:\.|::)[A-Za-z_][A-Za-z0-9_\-]*)*)`").expect("tick")
});

fn tick_plausible(name: &str) -> bool {
    name.chars().count() > 1
        && (name.contains('.')
            || name.contains("::")
            || name.contains('_')
            || name.chars().any(|c| c.is_uppercase()))
}

/// Symbols in order of first appearance, without duplicates.
pub fn extract_symbol_references(text: &str) -> Vec<String> {
    let mut marks: Vec<(usize, String)> = Vec::new();
    for m in AT_RE.captures_iter(text) {
        let raw = m.get(0).map(|g| g.as_str()).unwrap_or("");
        let name = symbol_ref(raw);
        if !name.is_empty() {
            marks.push((m.get(0).map(|g| g.start()).unwrap_or(0), name));
        }
    }
    for m in TICK_RE.captures_iter(text) {
        let name = m.get(1).map(|g| g.as_str()).unwrap_or("");
        if tick_plausible(name) {
            let norm = symbol_ref(name);
            if !norm.is_empty() {
                marks.push((m.get(0).map(|g| g.start()).unwrap_or(0), norm));
            }
        }
    }
    marks.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut out = Vec::new();
    for (_, name) in marks {
        if !out.iter().any(|s: &String| s == &name) {
            out.push(name);
        }
    }
    out
}

/// 16 hex digits, stable across runs. Inputs are NUL-separated to stay unambiguous.
pub fn generate_chunk_id(source_path: &str, section_title: &str, index: usize) -> String {
    format!(
        "{:016x}",
        hash_str(&format!("{source_path}\u{0}{section_title}\u{0}{index}"))
    )
}

fn line_starts(content: &str) -> Vec<usize> {
    let mut starts = vec![0];
    let bytes = content.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' && i + 1 < bytes.len() {
            starts.push(i + 1);
        }
    }
    starts
}

fn line_of(starts: &[usize], byte: usize) -> usize {
    match starts.binary_search(&byte) {
        Ok(i) => i + 1,
        Err(i) => i.max(1),
    }
}

fn heading_u32(level: HeadingLevel) -> u32 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

struct Heading {
    byte: usize,
    level: u32,
    title: String,
}

fn headings_of(content: &str) -> Vec<Heading> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_FOOTNOTES);
    let mut out = Vec::new();
    let mut current: Option<(usize, u32, String)> = None;
    for (ev, range) in Parser::new_ext(content, opts).into_offset_iter() {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                current = Some((range.start, heading_u32(level), String::new()));
            }
            Event::Text(t) | Event::Code(t) if current.is_some() => {
                if let Some(c) = current.as_mut() {
                    c.2.push_str(&t);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((byte, level, title)) = current.take() {
                    let title = title.trim().to_string();
                    if !title.is_empty() {
                        out.push(Heading { byte, level, title });
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn finish(
    source_path: &str,
    sections: Vec<(String, u32, Vec<String>, String, usize, usize)>,
) -> Vec<DocChunk> {
    sections
        .into_iter()
        .enumerate()
        .map(
            |(idx, (title, level, hierarchy, content, start, end))| DocChunk {
                id: generate_chunk_id(source_path, &title, idx),
                source_path: source_path.to_string(),
                symbol_references: extract_symbol_references(&content),
                section_title: title,
                heading_level: level,
                hierarchy,
                content,
                start_line: start,
                end_line: end.max(start),
            },
        )
        .collect()
}

/// Generic split (H1-H6 headings; code blocks are not treated as headings).
pub fn parse_markdown(source_path: &str, content: &str) -> Vec<DocChunk> {
    if content.trim().is_empty() {
        return Vec::new();
    }
    let starts = line_starts(content);
    let last_line = line_of(&starts, content.len().saturating_sub(1));
    let found = headings_of(content);
    if found.is_empty() {
        return finish(
            source_path,
            vec![(
                "Overview".into(),
                0,
                vec!["Overview".into()],
                content.trim().to_string(),
                1,
                last_line,
            )],
        );
    }

    let mut sections = Vec::new();
    let preamble = content[..found[0].byte].trim();
    if !preamble.is_empty() {
        let end = line_of(&starts, found[0].byte.saturating_sub(1));
        sections.push((
            "Overview".into(),
            0,
            vec!["Overview".into()],
            preamble.to_string(),
            1,
            end,
        ));
    }

    let mut stack: Vec<(u32, String)> = Vec::new();
    for (i, h) in found.iter().enumerate() {
        let end_byte = found.get(i + 1).map(|n| n.byte).unwrap_or(content.len());
        let slice = &content[h.byte..end_byte];
        let after_first = slice.find('\n').map(|p| &slice[p + 1..]).unwrap_or("");
        let empty_body = after_first.trim().is_empty();
        let is_last = i + 1 == found.len();
        if empty_body && is_last && (!sections.is_empty() || i > 0) {
            continue;
        }
        while stack.last().is_some_and(|top| top.0 >= h.level) {
            stack.pop();
        }
        stack.push((h.level, h.title.clone()));
        let hierarchy: Vec<String> = stack.iter().map(|s| s.1.clone()).collect();
        let body = if empty_body {
            h.title.clone()
        } else {
            slice.trim().to_string()
        };
        let start_line = line_of(&starts, h.byte);
        let end_line = if end_byte > h.byte {
            line_of(&starts, end_byte - 1)
        } else {
            start_line
        };
        sections.push((
            h.title.clone(),
            h.level,
            hierarchy,
            body,
            start_line,
            end_line,
        ));
    }
    if sections.is_empty() {
        return finish(
            source_path,
            vec![(
                found[0].title.clone(),
                found[0].level,
                vec![found[0].title.clone()],
                found[0].title.clone(),
                line_of(&starts, found[0].byte),
                line_of(&starts, found[0].byte),
            )],
        );
    }
    finish(source_path, sections)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_ids_are_stable_and_distinct() {
        let a = generate_chunk_id("docs/a.md", "Intro", 0);
        assert_eq!(a, generate_chunk_id("docs/a.md", "Intro", 0));
        assert_ne!(a, generate_chunk_id("docs/a.md", "Intro", 1));
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn symbol_refs_keep_order_and_drop_noise() {
        assert_eq!(symbol_ref("  @service: BillingApi "), "BillingApi");
        assert_eq!(symbol_ref("@not a ref"), "@not a ref");
        let text =
            "see @fn: PaymentProcessor, `order_total`, `true`, `npm`, `config.port` and `std::fs`";
        assert_eq!(
            extract_symbol_references(text),
            vec![
                "PaymentProcessor".to_string(),
                "order_total".to_string(),
                "config.port".to_string(),
                "std::fs".to_string(),
            ]
        );
    }

    #[test]
    fn empty_and_overview() {
        assert!(parse_markdown("a.txt", "  \n").is_empty());
        let ch = parse_markdown("a.txt", "just text\n");
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0].section_title, "Overview");
        assert_eq!(ch[0].heading_level, 0);
    }

    #[test]
    fn code_fence_is_not_a_heading_and_level_jump_stacks() {
        let md = "# Top\n\n```bash\n# not a title\n```\n\n### Child\n\nbody\n";
        let ch = parse_markdown("a.md", md);
        let titles: Vec<_> = ch.iter().map(|c| c.section_title.as_str()).collect();
        assert_eq!(titles, ["Top", "Child"]);
        assert_eq!(
            ch[1].hierarchy,
            vec!["Top".to_string(), "Child".to_string()]
        );
        assert!(
            !ch[0].content.contains("not a title")
                || ch.iter().all(|c| c.section_title != "not a title")
        );
    }

    #[test]
    fn empty_middle_kept_trailing_empty_dropped_crlf_lines() {
        let md = "# A\r\n\r\n# B\r\n\r\n# C\r\n";
        let ch = parse_markdown("a.md", md);
        let titles: Vec<_> = ch.iter().map(|c| c.section_title.as_str()).collect();
        assert_eq!(titles, ["A", "B"]);
        assert_eq!(ch[0].content, "A");
        assert_eq!(ch[0].start_line, 1);
        assert_eq!(ch[1].start_line, 3);
    }
}
