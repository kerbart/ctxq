//! Token recognizers.  They share mechanical helpers but keep language rules
//! in separate modules.

pub mod csharp;
pub mod go;
pub mod java;
pub mod kotlin;
pub mod python;
pub mod rust;
pub mod typescript;

use std::cmp::Reverse;

use super::lex::{Token, TokenKind};
use super::model::{Call, FileOutline, Import, Lang, Symbol, SymbolKind};
use super::scope::ScopeTree;

#[derive(Debug, Clone)]
pub(crate) struct Decl {
    pub name: String,
    pub kind: SymbolKind,
    pub start_tok: usize,
    pub end_tok: usize,
    pub sig_start: usize,
    pub sig_end: usize,
    pub body: Option<(usize, usize)>,
    pub callable: bool,
    pub parent_hint: Option<String>,
    pub doc: String,
}

#[derive(Debug, Default)]
pub(crate) struct Recognized {
    pub decls: Vec<Decl>,
    pub imports: Vec<Import>,
}

pub(crate) fn finish(
    lang: Lang,
    source: &str,
    tokens: &[Token],
    mut recognized: Recognized,
) -> FileOutline {
    recognized
        .decls
        .sort_by_key(|decl| (decl.start_tok, Reverse(decl.end_tok), decl.kind as u8));
    recognized.decls.dedup_by(|right, left| {
        left.start_tok == right.start_tok && left.kind == right.kind && left.name == right.name
    });

    let mut symbols: Vec<Symbol> = Vec::with_capacity(recognized.decls.len());
    let mut active: Vec<usize> = Vec::new();
    for (index, decl) in recognized.decls.iter().enumerate() {
        while active.last().is_some_and(|candidate| {
            !recognized.decls[*candidate]
                .body
                .is_some_and(|(start, end)| start <= decl.start_tok && decl.start_tok < end)
        }) {
            active.pop();
        }
        let enclosing = active.last().copied();

        let enclosing_qname = enclosing
            .and_then(|candidate| symbols.get(candidate))
            .map(|symbol| symbol.qname.clone());
        let parent_qname = if let Some(hint) = &decl.parent_hint {
            let namespace = enclosing.and_then(|candidate| {
                let raw = &recognized.decls[candidate];
                matches!(raw.kind, SymbolKind::Module).then(|| symbols[candidate].qname.clone())
            });
            Some(namespace.map_or_else(|| hint.clone(), |prefix| format!("{prefix}.{hint}")))
        } else {
            enclosing_qname
        };
        let qname = parent_qname.as_ref().map_or_else(
            || decl.name.clone(),
            |parent| format!("{parent}.{}", decl.name),
        );
        let mut kind = decl.kind;
        if enclosing.is_some_and(|candidate| is_type_kind(recognized.decls[candidate].kind)) {
            if kind == SymbolKind::Function {
                kind = SymbolKind::Method;
            } else if matches!(kind, SymbolKind::Variable | SymbolKind::Const) {
                kind = SymbolKind::Field;
            }
        }
        let start_line = tokens
            .get(decl.start_tok)
            .map_or(1, |token| token.line.max(1));
        let end_line = tokens
            .get(decl.end_tok)
            .map_or(start_line, |token| token.end_line.max(start_line));
        symbols.push(Symbol {
            name: decl.name.clone(),
            qname,
            kind,
            lang,
            start_line,
            end_line,
            signature: normalize_signature(source, decl.sig_start, decl.sig_end),
            doc: decl.doc.clone(),
            parent_qname,
        });
        if decl.body.is_some() {
            active.push(index);
        }
    }

    let calls = collect_calls(source, tokens, &recognized.decls, &symbols);
    recognized.imports.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.names.cmp(&right.names))
    });
    recognized.imports.dedup();
    FileOutline {
        symbols,
        calls,
        imports: recognized.imports,
    }
}

fn is_type_kind(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class
            | SymbolKind::Struct
            | SymbolKind::Enum
            | SymbolKind::Interface
            | SymbolKind::Trait
    )
}

fn collect_calls(source: &str, tokens: &[Token], decls: &[Decl], symbols: &[Symbol]) -> Vec<Call> {
    let mut starts: Vec<(usize, Reverse<usize>, usize)> = decls
        .iter()
        .enumerate()
        .filter_map(|(index, decl)| {
            (decl.callable)
                .then_some(decl.body)
                .flatten()
                .map(|(start, end)| (start, Reverse(end), index))
        })
        .collect();
    starts.sort_unstable();
    let mut calls = Vec::new();
    let mut active: Vec<usize> = Vec::new();
    let mut next_start = 0usize;
    for (open, token) in tokens.iter().enumerate() {
        while active
            .last()
            .is_some_and(|index| decls[*index].body.is_some_and(|(_, end)| open >= end))
        {
            active.pop();
        }
        while starts
            .get(next_start)
            .is_some_and(|(start, _, _)| *start == open)
        {
            active.push(starts[next_start].2);
            next_start += 1;
        }
        if token.text(source) != "(" {
            continue;
        }
        let Some(caller) = active.last().copied() else {
            continue;
        };
        let Some((callee, line)) = callee_before(source, tokens, open) else {
            continue;
        };
        calls.push(Call {
            caller_qname: symbols[caller].qname.clone(),
            callee,
            line,
        });
    }
    calls.sort_by(|left, right| {
        left.line
            .cmp(&right.line)
            .then_with(|| left.caller_qname.cmp(&right.caller_qname))
            .then_with(|| left.callee.cmp(&right.callee))
    });
    calls.dedup();
    calls
}

fn callee_before(source: &str, tokens: &[Token], open: usize) -> Option<(String, u32)> {
    let mut at = prev_sig(tokens, open)?;
    if text(source, tokens, at) == ">" {
        at = before_angle_group(source, tokens, at)?;
        if text(source, tokens, at) == ":" {
            at = prev_sig(tokens, at)?;
            if text(source, tokens, at) == ":" {
                at = prev_sig(tokens, at)?;
            }
        }
    }
    if text(source, tokens, at) == "]" {
        at = before_square_group(source, tokens, at)?;
    }
    if text(source, tokens, at) == "!" {
        at = prev_sig(tokens, at)?;
    }
    if tokens.get(at)?.kind != TokenKind::Ident {
        return None;
    }
    let terminal = text(source, tokens, at);
    if is_call_keyword(terminal) {
        return None;
    }
    let line = tokens[at].line;
    let mut start = at;
    while let Some(separator_end) = prev_sig(tokens, start) {
        let separator = text(source, tokens, separator_end);
        let ident_at = if separator == "." {
            prev_sig(tokens, separator_end)
        } else if separator == ":" {
            let Some(first_colon) = prev_sig(tokens, separator_end) else {
                break;
            };
            if text(source, tokens, first_colon) != ":" {
                break;
            }
            prev_sig(tokens, first_colon)
        } else {
            break;
        };
        let Some(ident_at) = ident_at else { break };
        if tokens[ident_at].kind != TokenKind::Ident {
            break;
        }
        start = ident_at;
    }
    let mut callee = String::new();
    for token in &tokens[start..=at] {
        if token.kind != TokenKind::Newline {
            callee.push_str(token.text(source));
        }
    }
    (!callee.is_empty()).then_some((callee, line))
}

fn before_square_group(source: &str, tokens: &[Token], mut at: usize) -> Option<usize> {
    let mut depth = 0usize;
    loop {
        match text(source, tokens, at) {
            "]" => depth = depth.saturating_add(1),
            "[" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return prev_sig(tokens, at);
                }
            }
            _ => {}
        }
        at = prev_sig(tokens, at)?;
    }
}

fn before_angle_group(source: &str, tokens: &[Token], mut at: usize) -> Option<usize> {
    let mut depth = 0usize;
    loop {
        match text(source, tokens, at) {
            ">" => depth = depth.saturating_add(1),
            "<" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return prev_sig(tokens, at);
                }
            }
            _ => {}
        }
        at = prev_sig(tokens, at)?;
    }
}

fn is_call_keyword(value: &str) -> bool {
    matches!(
        value,
        "if" | "for"
            | "while"
            | "switch"
            | "catch"
            | "match"
            | "return"
            | "sizeof"
            | "typeof"
            | "nameof"
            | "checked"
            | "unchecked"
            | "lock"
            | "using"
            | "with"
            | "when"
            | "assert"
            | "super"
            | "this"
    )
}

pub(crate) fn text<'a>(source: &'a str, tokens: &[Token], index: usize) -> &'a str {
    tokens.get(index).map_or("", |token| token.text(source))
}

pub(crate) fn is(source: &str, tokens: &[Token], index: usize, expected: &str) -> bool {
    text(source, tokens, index) == expected
}

pub(crate) fn next_sig(tokens: &[Token], mut index: usize) -> Option<usize> {
    index = index.saturating_add(1);
    while let Some(token) = tokens.get(index) {
        if !matches!(token.kind, TokenKind::Newline | TokenKind::DocComment) {
            return Some(index);
        }
        index += 1;
    }
    None
}

pub(crate) fn prev_sig(tokens: &[Token], mut index: usize) -> Option<usize> {
    while index > 0 {
        index -= 1;
        if !matches!(
            tokens[index].kind,
            TokenKind::Newline | TokenKind::DocComment
        ) {
            return Some(index);
        }
    }
    None
}

pub(crate) fn ident(source: &str, tokens: &[Token], index: usize) -> Option<String> {
    (tokens.get(index)?.kind == TokenKind::Ident).then(|| {
        text(source, tokens, index)
            .strip_prefix('`')
            .and_then(|value| value.strip_suffix('`'))
            .unwrap_or_else(|| text(source, tokens, index))
            .to_owned()
    })
}

pub(crate) fn matching(tree: &ScopeTree, index: usize, fallback: usize) -> usize {
    tree.matching(index).unwrap_or(fallback)
}

pub(crate) fn find_body_or_end(
    source: &str,
    tokens: &[Token],
    tree: &ScopeTree,
    mut index: usize,
) -> (Option<(usize, usize)>, usize, usize) {
    let fallback = tokens.len().saturating_sub(1);
    while index < tokens.len() {
        match text(source, tokens, index) {
            "{" => {
                let close = matching(tree, index, fallback);
                return (
                    Some((index.saturating_add(1), close)),
                    close,
                    tokens[index].start,
                );
            }
            ";" => return (None, index, tokens[index].start),
            _ => index += 1,
        }
    }
    (None, fallback, source.len())
}

pub(crate) fn expression_body(
    source: &str,
    tokens: &[Token],
    start: usize,
) -> (Option<(usize, usize)>, usize) {
    let mut at = start;
    while at < tokens.len() {
        if is(source, tokens, at, ";") {
            return (Some((start, at)), at);
        }
        if tokens[at].kind == TokenKind::Newline {
            return (Some((start, at)), at.saturating_sub(1).max(start));
        }
        at += 1;
    }
    let end = tokens.len().saturating_sub(1);
    (Some((start.min(end), tokens.len())), end)
}

pub(crate) fn docs_before(source: &str, tokens: &[Token], start_tok: usize) -> String {
    let declaration_line = tokens.get(start_tok).map_or(1, |token| token.line);
    let mut indexes = Vec::new();
    let mut at = start_tok;
    let mut nearest_line = declaration_line;
    while at > 0 {
        at -= 1;
        let token = &tokens[at];
        match token.kind {
            TokenKind::Newline => continue,
            TokenKind::DocComment if token.end_line.saturating_add(1) >= nearest_line => {
                nearest_line = token.line;
                indexes.push(at);
            }
            _ => break,
        }
    }
    indexes.reverse();
    indexes
        .into_iter()
        .map(|index| clean_doc(tokens[index].text(source)))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn clean_doc(raw: &str) -> String {
    let mut value = raw.trim();
    for marker in ["///", "//!", "/**", "/*!", "/*", "*/"] {
        value = value.strip_prefix(marker).unwrap_or(value);
        value = value.strip_suffix(marker).unwrap_or(value);
    }
    value
        .lines()
        .map(|line| line.trim().strip_prefix('*').unwrap_or(line.trim()).trim())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

pub(crate) fn string_value(raw: &str) -> String {
    let Some(first_quote) = raw.find(['\'', '"', '`']) else {
        return raw.to_owned();
    };
    let quote = raw.as_bytes()[first_quote];
    let triple = raw
        .as_bytes()
        .get(first_quote..first_quote.saturating_add(3))
        == Some(&[quote, quote, quote]);
    let width = if triple { 3 } else { 1 };
    let start = first_quote + width;
    let end = raw.len().saturating_sub(width).max(start);
    raw.get(start..end).unwrap_or("").to_owned()
}

pub(crate) fn declaration_start(
    source: &str,
    tokens: &[Token],
    keyword: usize,
    modifiers: &[&str],
) -> usize {
    let mut start = keyword;
    while let Some(previous) = prev_sig(tokens, start) {
        let value = text(source, tokens, previous);
        if modifiers.contains(&value)
            && tokens[previous].line.saturating_add(4) >= tokens[start].line
        {
            start = previous;
            continue;
        }
        if value == "]" {
            let mut depth = 1usize;
            let mut cursor = previous;
            while let Some(candidate) = prev_sig(tokens, cursor) {
                cursor = candidate;
                match text(source, tokens, cursor) {
                    "]" => depth = depth.saturating_add(1),
                    "[" => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            start = cursor;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if start == cursor {
                continue;
            }
        }
        break;
    }
    start
}

pub(crate) fn normalize_signature(source: &str, start: usize, end: usize) -> String {
    let value = source
        .get(start.min(source.len())..end.min(source.len()))
        .unwrap_or("");
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized = strip_leading_attributes(&normalized).to_string();
    if normalized.chars().count() <= 300 {
        return normalized;
    }
    normalized
        .chars()
        .take(299)
        .chain(std::iter::once('…'))
        .collect()
}

/// Drops leading `#[..]` / `[..]` attribute groups (Rust, C#): they stay in the
/// symbol's line range but are noise in a signature.
fn strip_leading_attributes(mut sig: &str) -> &str {
    loop {
        let rest = sig.strip_prefix('#').unwrap_or(sig).trim_start();
        let Some(inner) = rest.strip_prefix('[') else {
            return sig;
        };
        let mut depth = 1usize;
        let mut cut = None;
        for (i, c) in inner.char_indices() {
            match c {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        cut = Some(i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        match cut {
            Some(i) if !inner[i..].trim().is_empty() => sig = inner[i..].trim_start(),
            _ => return sig,
        }
    }
}

pub(crate) fn import(path: String, names: Vec<String>) -> Import {
    Import { path, names }
}

pub(crate) fn comma_names(source: &str, tokens: &[Token], start: usize, end: usize) -> Vec<String> {
    let mut names = Vec::new();
    let mut at = start;
    while at < end && at < tokens.len() {
        if tokens[at].kind == TokenKind::Ident {
            let value = text(source, tokens, at);
            if !matches!(value, "as" | "static" | "type") {
                names.push(value.to_owned());
            }
        }
        at += 1;
    }
    names
}

pub(crate) fn brace_depths(source: &str, tokens: &[Token]) -> Vec<usize> {
    let mut depths = Vec::with_capacity(tokens.len());
    let mut depth = 0usize;
    for token in tokens {
        depths.push(depth);
        match token.text(source) {
            "{" => depth = depth.saturating_add(1),
            "}" => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    depths
}

pub(crate) fn push_semicolon_fields(source: &str, tokens: &[Token], recognized: &mut Recognized) {
    let bodies: Vec<(usize, usize)> = recognized
        .decls
        .iter()
        .filter(|decl| is_type_kind(decl.kind))
        .filter_map(|decl| decl.body)
        .collect();
    if bodies.is_empty() {
        return;
    }
    let depths = brace_depths(source, tokens);
    for (body_start, body_end) in bodies {
        let Some(base_depth) = depths.get(body_start).copied() else {
            continue;
        };
        for end in body_start..body_end.min(tokens.len()) {
            if depths.get(end) != Some(&base_depth) || !is(source, tokens, end, ";") {
                continue;
            }
            let mut start = end;
            while start > body_start {
                let previous = start - 1;
                if depths.get(previous) != Some(&base_depth)
                    || matches!(text(source, tokens, previous), ";" | "{" | "}")
                {
                    break;
                }
                start = previous;
            }
            while start < end
                && matches!(
                    tokens[start].kind,
                    TokenKind::Newline | TokenKind::DocComment
                )
            {
                start += 1;
            }
            if start >= end
                || (start..end).any(|index| {
                    matches!(
                        text(source, tokens, index),
                        "class" | "record" | "struct" | "interface" | "enum" | "return" | "throw"
                    )
                })
            {
                continue;
            }
            let equals = (start..end).find(|index| is(source, tokens, *index, "="));
            if (start..equals.unwrap_or(end)).any(|index| is(source, tokens, index, "(")) {
                continue;
            }
            let before = equals.unwrap_or(end);
            let Some(name_at) = (start..before)
                .rev()
                .find(|index| tokens[*index].kind == TokenKind::Ident)
            else {
                continue;
            };
            let name = ident(source, tokens, name_at).unwrap_or_default();
            if name.is_empty() || is_field_modifier(&name) {
                continue;
            }
            recognized.decls.push(Decl {
                name,
                kind: SymbolKind::Field,
                start_tok: start,
                end_tok: end,
                sig_start: tokens[start].start,
                sig_end: tokens[end].start,
                body: None,
                callable: false,
                parent_hint: None,
                doc: docs_before(source, tokens, start),
            });
        }
    }
}

fn is_field_modifier(value: &str) -> bool {
    matches!(
        value,
        "public"
            | "private"
            | "protected"
            | "internal"
            | "static"
            | "final"
            | "readonly"
            | "const"
            | "volatile"
            | "transient"
    )
}

#[cfg(test)]
mod signature_tests {
    use super::strip_leading_attributes;

    #[test]
    fn attributes_are_not_part_of_the_signature() {
        assert_eq!(
            strip_leading_attributes("#[allow(clippy::x)] #[inline] pub fn f()"),
            "pub fn f()"
        );
        assert_eq!(
            strip_leading_attributes("[HttpGet(\"a[0]\")] public int Get()"),
            "public int Get()"
        );
        assert_eq!(strip_leading_attributes("pub fn g()"), "pub fn g()");
        assert_eq!(strip_leading_attributes("[unclosed"), "[unclosed");
    }
}
