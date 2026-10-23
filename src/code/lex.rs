//! Small, table-driven lexer used by every code recognizer.
//!
//! It deliberately does not try to assign grammatical meaning to tokens.  Its
//! main job is to make braces and declaration-looking words inside comments and
//! literals invisible to the recognizers.

use super::model::Lang;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Ident,
    Number,
    String,
    Char,
    DocComment,
    Punct,
    Newline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub start: usize,
    pub end: usize,
    pub line: u32,
    pub end_line: u32,
    pub column: u32,
}

impl Token {
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        source.get(self.start..self.end).unwrap_or("")
    }
}

#[derive(Debug, Clone)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub line_count: u32,
}

/// Lexical switches which differ between supported languages.
#[derive(Debug, Clone, Copy)]
pub struct LangSpec {
    pub line_comment: Option<&'static str>,
    pub block_comment: Option<(&'static str, &'static str)>,
    pub nested_block_comments: bool,
    pub doc_line_markers: &'static [&'static str],
    pub doc_block_markers: &'static [&'static str],
    pub hash_comments: bool,
    pub backtick_strings: bool,
    pub backtick_identifiers: bool,
    pub template_strings: bool,
    pub triple_strings: bool,
    pub csharp_strings: bool,
    pub rust_raw_strings: bool,
    pub python_prefixes: bool,
}

const NONE: &[&str] = &[];
const RUST_DOC_LINE: &[&str] = &["///", "//!"];
const RUST_DOC_BLOCK: &[&str] = &["/**", "/*!"];
const C_DOC_BLOCK: &[&str] = &["/**"];
const CSHARP_DOC_LINE: &[&str] = &["///"];
const GO_DOC_LINE: &[&str] = &["//"];
const GO_DOC_BLOCK: &[&str] = &["/*"];

pub fn spec(lang: Lang) -> LangSpec {
    match lang {
        Lang::Rust => LangSpec {
            line_comment: Some("//"),
            block_comment: Some(("/*", "*/")),
            nested_block_comments: true,
            doc_line_markers: RUST_DOC_LINE,
            doc_block_markers: RUST_DOC_BLOCK,
            hash_comments: false,
            backtick_strings: false,
            backtick_identifiers: false,
            template_strings: false,
            triple_strings: false,
            csharp_strings: false,
            rust_raw_strings: true,
            python_prefixes: false,
        },
        Lang::TypeScript | Lang::JavaScript => LangSpec {
            line_comment: Some("//"),
            block_comment: Some(("/*", "*/")),
            nested_block_comments: false,
            doc_line_markers: NONE,
            doc_block_markers: C_DOC_BLOCK,
            hash_comments: false,
            backtick_strings: true,
            backtick_identifiers: false,
            template_strings: true,
            triple_strings: false,
            csharp_strings: false,
            rust_raw_strings: false,
            python_prefixes: false,
        },
        Lang::Python => LangSpec {
            line_comment: None,
            block_comment: None,
            nested_block_comments: false,
            doc_line_markers: NONE,
            doc_block_markers: NONE,
            hash_comments: true,
            backtick_strings: false,
            backtick_identifiers: false,
            template_strings: false,
            triple_strings: true,
            csharp_strings: false,
            rust_raw_strings: false,
            python_prefixes: true,
        },
        Lang::Go => LangSpec {
            line_comment: Some("//"),
            block_comment: Some(("/*", "*/")),
            nested_block_comments: false,
            doc_line_markers: GO_DOC_LINE,
            doc_block_markers: GO_DOC_BLOCK,
            hash_comments: false,
            backtick_strings: true,
            backtick_identifiers: false,
            template_strings: false,
            triple_strings: false,
            csharp_strings: false,
            rust_raw_strings: false,
            python_prefixes: false,
        },
        Lang::Java => LangSpec {
            line_comment: Some("//"),
            block_comment: Some(("/*", "*/")),
            nested_block_comments: false,
            doc_line_markers: NONE,
            doc_block_markers: C_DOC_BLOCK,
            hash_comments: false,
            backtick_strings: false,
            backtick_identifiers: false,
            template_strings: false,
            triple_strings: true,
            csharp_strings: false,
            rust_raw_strings: false,
            python_prefixes: false,
        },
        Lang::Kotlin => LangSpec {
            line_comment: Some("//"),
            block_comment: Some(("/*", "*/")),
            nested_block_comments: true,
            doc_line_markers: NONE,
            doc_block_markers: C_DOC_BLOCK,
            hash_comments: false,
            backtick_strings: false,
            backtick_identifiers: true,
            template_strings: false,
            triple_strings: true,
            csharp_strings: false,
            rust_raw_strings: false,
            python_prefixes: false,
        },
        Lang::CSharp => LangSpec {
            line_comment: Some("//"),
            block_comment: Some(("/*", "*/")),
            nested_block_comments: false,
            doc_line_markers: CSHARP_DOC_LINE,
            doc_block_markers: C_DOC_BLOCK,
            hash_comments: false,
            backtick_strings: false,
            backtick_identifiers: false,
            template_strings: false,
            triple_strings: true,
            csharp_strings: true,
            rust_raw_strings: false,
            python_prefixes: false,
        },
    }
}

pub fn lex(lang: Lang, source: &str) -> Lexed {
    let spec = spec(lang);
    let bytes = source.as_bytes();
    let mut tokens = Vec::with_capacity(source.len() / 6);
    let mut i = 0usize;
    let mut line = 1u32;
    let mut column = 1u32;

    while i < bytes.len() {
        let start = i;
        let start_line = line;
        let start_column = column;
        let byte = bytes[i];

        if byte == b'\n' || byte == b'\r' {
            if byte == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                i += 2;
            } else {
                i += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Newline,
                start,
                end: i,
                line,
                end_line: line,
                column,
            });
            line = line.saturating_add(1);
            column = 1;
            continue;
        }
        if byte.is_ascii_whitespace() {
            i += 1;
            column = column.saturating_add(1);
            continue;
        }

        if spec.hash_comments && byte == b'#' {
            i = scan_to_newline(bytes, i + 1);
            column = column.saturating_add((i - start) as u32);
            continue;
        }

        if byte == b'/' {
            if let Some(marker) = matching_marker(source, i, spec.doc_line_markers) {
                i = scan_to_newline(bytes, i + marker.len());
                tokens.push(Token {
                    kind: TokenKind::DocComment,
                    start,
                    end: i,
                    line: start_line,
                    end_line: line,
                    column: start_column,
                });
                column = column.saturating_add((i - start) as u32);
                continue;
            }
            if let Some(marker) = matching_marker(source, i, spec.doc_block_markers) {
                if let Some((open, close)) = spec.block_comment {
                    let end = scan_block_comment(
                        source,
                        i + marker.len().max(open.len()),
                        open,
                        close,
                        spec.nested_block_comments,
                    );
                    advance_position(bytes, i, end, &mut line, &mut column);
                    i = end;
                    tokens.push(Token {
                        kind: TokenKind::DocComment,
                        start,
                        end: i,
                        line: start_line,
                        end_line: line,
                        column: start_column,
                    });
                    continue;
                }
            }
            if let Some(marker) = spec.line_comment.filter(|m| starts_with(source, i, m)) {
                i = scan_to_newline(bytes, i + marker.len());
                column = column.saturating_add((i - start) as u32);
                continue;
            }
            if let Some((open, close)) = spec
                .block_comment
                .filter(|(open, _)| starts_with(source, i, open))
            {
                let end = scan_block_comment(
                    source,
                    i + open.len(),
                    open,
                    close,
                    spec.nested_block_comments,
                );
                advance_position(bytes, i, end, &mut line, &mut column);
                i = end;
                continue;
            }
        }

        if spec.backtick_identifiers && byte == b'`' {
            let end = scan_quoted(source, i, b'`', false, false);
            advance_position(bytes, i, end, &mut line, &mut column);
            i = end.max(i + 1).min(bytes.len());
            tokens.push(Token {
                kind: TokenKind::Ident,
                start,
                end: i,
                line: start_line,
                end_line: line,
                column: start_column,
            });
            continue;
        }

        if might_start_string(lang, byte) {
            if let Some((end, kind)) = scan_string(&spec, lang, source, i) {
                advance_position(bytes, i, end, &mut line, &mut column);
                i = end.max(i + 1).min(bytes.len());
                tokens.push(Token {
                    kind,
                    start,
                    end: i,
                    line: start_line,
                    end_line: line,
                    column: start_column,
                });
                continue;
            }
        }

        if byte == b'_' || byte == b'$' || byte.is_ascii_alphabetic() {
            i = scan_identifier_end(source, i);
            column = column.saturating_add((i - start) as u32);
            tokens.push(Token {
                kind: TokenKind::Ident,
                start,
                end: i,
                line,
                end_line: line,
                column: start_column,
            });
            continue;
        }
        if byte.is_ascii_digit() {
            i += 1;
            while let Some(next) = bytes.get(i).copied() {
                if !(next.is_ascii_alphanumeric() || matches!(next, b'_' | b'.')) {
                    break;
                }
                i += 1;
            }
            column = column.saturating_add((i - start) as u32);
            tokens.push(Token {
                kind: TokenKind::Number,
                start,
                end: i,
                line,
                end_line: line,
                column: start_column,
            });
            continue;
        }

        let Some(ch) = source.get(i..).and_then(|tail| tail.chars().next()) else {
            break;
        };
        if is_ident_start(ch) {
            i = scan_identifier_end(source, i);
            column = column.saturating_add((i - start) as u32);
            tokens.push(Token {
                kind: TokenKind::Ident,
                start,
                end: i,
                line,
                end_line: line,
                column: start_column,
            });
            continue;
        }

        i += ch.len_utf8();
        column = column.saturating_add(1);
        tokens.push(Token {
            kind: TokenKind::Punct,
            start,
            end: i,
            line,
            end_line: line,
            column: start_column,
        });
    }

    Lexed {
        tokens,
        line_count: line.max(1),
    }
}

fn might_start_string(lang: Lang, byte: u8) -> bool {
    matches!(byte, b'\'' | b'"' | b'`')
        || (lang == Lang::Rust && matches!(byte, b'r' | b'b'))
        || (lang == Lang::Python
            && matches!(byte, b'r' | b'R' | b'b' | b'B' | b'u' | b'U' | b'f' | b'F'))
        || (lang == Lang::CSharp && matches!(byte, b'@' | b'$'))
}

fn scan_identifier_end(source: &str, mut at: usize) -> usize {
    let bytes = source.as_bytes();
    while let Some(byte) = bytes.get(at).copied() {
        if byte == b'_' || byte == b'$' || byte.is_ascii_alphanumeric() {
            at += 1;
            continue;
        }
        if byte.is_ascii() {
            break;
        }
        let Some(ch) = source.get(at..).and_then(|tail| tail.chars().next()) else {
            break;
        };
        if !is_ident_continue(ch) {
            break;
        }
        at += ch.len_utf8();
    }
    at
}

fn matching_marker<'a>(source: &str, at: usize, markers: &'a [&'a str]) -> Option<&'a str> {
    markers
        .iter()
        .copied()
        .filter(|marker| starts_with(source, at, marker))
        .max_by_key(|marker| marker.len())
}

fn starts_with(source: &str, at: usize, value: &str) -> bool {
    source.as_bytes().get(at..at.saturating_add(value.len())) == Some(value.as_bytes())
}

fn scan_to_newline(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() && !matches!(bytes[at], b'\n' | b'\r') {
        at += 1;
    }
    at
}

fn scan_block_comment(source: &str, mut at: usize, open: &str, close: &str, nested: bool) -> usize {
    let mut depth = 1usize;
    while at < source.len() {
        if starts_with(source, at, close) {
            at += close.len();
            depth -= 1;
            if depth == 0 {
                return at;
            }
            continue;
        }
        if nested && starts_with(source, at, open) {
            depth = depth.saturating_add(1);
            at += open.len();
            continue;
        }
        at += next_char_len(source, at);
    }
    source.len()
}

fn scan_string(spec: &LangSpec, lang: Lang, source: &str, at: usize) -> Option<(usize, TokenKind)> {
    if spec.rust_raw_strings {
        if let Some(end) = scan_rust_raw(source, at) {
            return Some((end, TokenKind::String));
        }
    }
    if spec.csharp_strings {
        if let Some(end) = scan_csharp_string(source, at) {
            return Some((end, TokenKind::String));
        }
    }
    if spec.python_prefixes {
        if let Some((quote_at, quote, triple, interpolated)) = python_string_start(source, at) {
            let end = if interpolated {
                scan_interpolated(source, quote_at, quote, triple, false, false)
            } else {
                scan_quoted(source, quote_at, quote, triple, false)
            };
            return Some((end, TokenKind::String));
        }
    }
    if spec.triple_strings && starts_with(source, at, "\"\"\"") {
        if lang == Lang::Kotlin {
            return Some((
                scan_interpolated(source, at, b'"', true, false, true),
                TokenKind::String,
            ));
        }
        return Some((
            scan_quoted(source, at, b'"', true, false),
            TokenKind::String,
        ));
    }
    if spec.template_strings && source.as_bytes().get(at) == Some(&b'`') {
        return Some((scan_template(source, at), TokenKind::String));
    }
    if spec.backtick_strings && source.as_bytes().get(at) == Some(&b'`') {
        return Some((
            scan_quoted(source, at, b'`', false, true),
            TokenKind::String,
        ));
    }
    if lang == Lang::Kotlin && source.as_bytes().get(at) == Some(&b'"') {
        return Some((
            scan_interpolated(source, at, b'"', false, false, true),
            TokenKind::String,
        ));
    }

    match source.as_bytes().get(at).copied()? {
        b'"' => Some((
            scan_quoted(source, at, b'"', false, false),
            TokenKind::String,
        )),
        b'\'' => {
            if lang == Lang::Rust && !looks_like_rust_char(source, at) {
                None
            } else {
                let kind = if lang == Lang::Python {
                    TokenKind::String
                } else {
                    TokenKind::Char
                };
                Some((scan_quoted(source, at, b'\'', false, false), kind))
            }
        }
        _ => None,
    }
}

fn scan_rust_raw(source: &str, at: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut cursor = at;
    if bytes.get(cursor) == Some(&b'b') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;
    let hash_start = cursor;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    let hashes = cursor - hash_start;
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }
    cursor += 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"'
            && bytes
                .get(cursor + 1..cursor + 1 + hashes)
                .is_some_and(|tail| tail.iter().all(|byte| *byte == b'#'))
        {
            return Some((cursor + 1 + hashes).min(bytes.len()));
        }
        cursor += 1;
    }
    Some(bytes.len())
}

fn scan_csharp_string(source: &str, at: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let (quote_at, verbatim, interpolated) = match bytes.get(at..at.saturating_add(3)) {
        Some([b'$', b'@', b'"']) | Some([b'@', b'$', b'"']) => (at + 2, true, true),
        _ if bytes.get(at..at.saturating_add(2)) == Some(b"@\"") => (at + 1, true, false),
        _ if bytes.get(at..at.saturating_add(2)) == Some(b"$\"") => (at + 1, false, true),
        _ => return None,
    };
    Some(if interpolated {
        scan_interpolated(source, quote_at, b'"', false, verbatim, false)
    } else {
        scan_quoted(source, quote_at, b'"', false, verbatim)
    })
}

fn python_string_start(source: &str, at: usize) -> Option<(usize, u8, bool, bool)> {
    let bytes = source.as_bytes();
    let mut cursor = at;
    let mut prefix_len = 0usize;
    while prefix_len < 3
        && bytes
            .get(cursor)
            .is_some_and(|byte| matches!(byte.to_ascii_lowercase(), b'r' | b'b' | b'u' | b'f'))
    {
        cursor += 1;
        prefix_len += 1;
    }
    let quote = *bytes.get(cursor)?;
    if !matches!(quote, b'\'' | b'"') {
        return None;
    }
    if prefix_len > 0
        && at > 0
        && source
            .get(..at)
            .and_then(|head| head.chars().next_back())
            .is_some_and(is_ident_continue)
    {
        return None;
    }
    let triple = bytes.get(cursor..cursor.saturating_add(3)) == Some(&[quote, quote, quote]);
    let interpolated = source
        .get(at..cursor)
        .is_some_and(|prefix| prefix.bytes().any(|byte| byte.eq_ignore_ascii_case(&b'f')));
    Some((cursor, quote, triple, interpolated))
}

fn scan_interpolated(
    source: &str,
    quote_at: usize,
    quote: u8,
    triple: bool,
    verbatim: bool,
    dollar_braces: bool,
) -> usize {
    let bytes = source.as_bytes();
    let width = if triple { 3 } else { 1 };
    let mut cursor = quote_at.saturating_add(width).min(bytes.len());
    while cursor < bytes.len() {
        if !verbatim && bytes[cursor] == b'\\' {
            cursor += 1;
            if cursor < bytes.len() {
                cursor += next_char_len(source, cursor);
            }
            continue;
        }
        if verbatim && !triple && bytes[cursor] == quote && bytes.get(cursor + 1) == Some(&quote) {
            cursor += 2;
            continue;
        }
        if triple {
            if bytes.get(cursor..cursor.saturating_add(3)) == Some(&[quote, quote, quote]) {
                return (cursor + 3).min(bytes.len());
            }
        } else if bytes[cursor] == quote {
            return cursor + 1;
        }

        let expression_start = if dollar_braces && bytes[cursor..].starts_with(b"${") {
            Some(cursor + 2)
        } else if !dollar_braces && bytes[cursor] == b'{' {
            if bytes.get(cursor + 1) == Some(&b'{') {
                cursor += 2;
                continue;
            }
            Some(cursor + 1)
        } else {
            None
        };
        if let Some(expression_start) = expression_start {
            cursor = scan_braced_expression(source, expression_start);
            continue;
        }
        cursor += next_char_len(source, cursor);
    }
    bytes.len()
}

fn scan_braced_expression(source: &str, mut cursor: usize) -> usize {
    let bytes = source.as_bytes();
    let mut depth = 1usize;
    while cursor < bytes.len() {
        if matches!(bytes[cursor], b'\'' | b'"') {
            let triple = bytes.get(cursor..cursor.saturating_add(3))
                == Some(&[bytes[cursor], bytes[cursor], bytes[cursor]]);
            cursor = scan_quoted(source, cursor, bytes[cursor], triple, false);
            continue;
        }
        if bytes[cursor] == b'`' {
            cursor = scan_template(source, cursor);
            continue;
        }
        if bytes[cursor..].starts_with(b"//") {
            cursor = scan_to_newline(bytes, cursor + 2);
            continue;
        }
        if bytes[cursor..].starts_with(b"/*") {
            cursor = scan_block_comment(source, cursor + 2, "/*", "*/", false);
            continue;
        }
        match bytes[cursor] {
            b'{' => depth = depth.saturating_add(1),
            b'}' => {
                depth = depth.saturating_sub(1);
                cursor += 1;
                if depth == 0 {
                    return cursor;
                }
                continue;
            }
            _ => {}
        }
        cursor += next_char_len(source, cursor);
    }
    bytes.len()
}

fn scan_quoted(source: &str, quote_at: usize, quote: u8, triple: bool, doubled: bool) -> usize {
    let bytes = source.as_bytes();
    let width = if triple { 3 } else { 1 };
    let mut cursor = quote_at.saturating_add(width).min(bytes.len());
    while cursor < bytes.len() {
        if !doubled && bytes[cursor] == b'\\' {
            cursor += 1;
            if cursor < bytes.len() {
                cursor += next_char_len(source, cursor);
            }
            continue;
        }
        if triple {
            if bytes.get(cursor..cursor.saturating_add(3)) == Some(&[quote, quote, quote]) {
                return (cursor + 3).min(bytes.len());
            }
        } else if bytes[cursor] == quote {
            if doubled && bytes.get(cursor + 1) == Some(&quote) {
                cursor += 2;
                continue;
            }
            return cursor + 1;
        }
        cursor += next_char_len(source, cursor);
    }
    bytes.len()
}

/// Scans nested `${ ... }` sections and nested template literals iteratively.
fn scan_template(source: &str, at: usize) -> usize {
    let bytes = source.as_bytes();
    let mut cursor = at.saturating_add(1);
    let mut modes: Vec<Option<usize>> = vec![None];
    while cursor < bytes.len() {
        let Some(mode) = modes.last_mut() else {
            return cursor;
        };
        if mode.is_none() {
            if bytes[cursor] == b'\\' {
                cursor = (cursor + 2).min(bytes.len());
            } else if bytes[cursor..].starts_with(b"${") {
                *mode = Some(1);
                cursor += 2;
            } else if bytes[cursor] == b'`' {
                cursor += 1;
                modes.pop();
                if modes.is_empty() {
                    return cursor;
                }
            } else {
                cursor += next_char_len(source, cursor);
            }
            continue;
        }

        if bytes[cursor] == b'`' {
            modes.push(None);
            cursor += 1;
        } else if matches!(bytes[cursor], b'\'' | b'"') {
            cursor = scan_quoted(source, cursor, bytes[cursor], false, false);
        } else if bytes[cursor..].starts_with(b"//") {
            cursor = scan_to_newline(bytes, cursor + 2);
        } else if bytes[cursor..].starts_with(b"/*") {
            cursor = scan_block_comment(source, cursor + 2, "/*", "*/", false);
        } else if bytes[cursor] == b'{' {
            if let Some(depth) = modes.last_mut().and_then(Option::as_mut) {
                *depth = depth.saturating_add(1);
            }
            cursor += 1;
        } else if bytes[cursor] == b'}' {
            if let Some(depth) = modes.last_mut().and_then(Option::as_mut) {
                *depth = depth.saturating_sub(1);
                if *depth == 0 {
                    if let Some(current) = modes.last_mut() {
                        *current = None;
                    }
                }
            }
            cursor += 1;
        } else {
            cursor += next_char_len(source, cursor);
        }
    }
    bytes.len()
}

fn looks_like_rust_char(source: &str, at: usize) -> bool {
    let bytes = source.as_bytes();
    let mut cursor = at.saturating_add(1);
    if cursor >= bytes.len() || matches!(bytes[cursor], b'\n' | b'\r' | b'\'') {
        return false;
    }
    if bytes[cursor] == b'\\' {
        cursor = cursor.saturating_add(2);
    } else {
        cursor += next_char_len(source, cursor);
    }
    bytes.get(cursor) == Some(&b'\'')
}

fn advance_position(bytes: &[u8], start: usize, end: usize, line: &mut u32, column: &mut u32) {
    for byte in bytes.get(start..end).unwrap_or_default() {
        if *byte == b'\n' {
            *line = line.saturating_add(1);
            *column = 1;
        } else {
            *column = column.saturating_add(1);
        }
    }
}

fn next_char_len(source: &str, at: usize) -> usize {
    source
        .get(at..)
        .and_then(|tail| tail.chars().next())
        .map_or(1, char::len_utf8)
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch == '$' || ch.is_alphabetic() || (!ch.is_ascii() && !ch.is_whitespace())
}

fn is_ident_continue(ch: char) -> bool {
    is_ident_start(ch) || ch.is_ascii_digit()
}
