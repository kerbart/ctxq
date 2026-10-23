use super::{clean_doc, ident, import, is, next_sig, string_value, text, Decl, Recognized};
use crate::code::lex::{Token, TokenKind};
use crate::code::model::SymbolKind;
use crate::code::scope::ScopeTree;

pub(crate) fn recognize(source: &str, tokens: &[Token], _tree: &ScopeTree) -> Recognized {
    let mut result = Recognized::default();
    let mut at = 0usize;
    while at < tokens.len() {
        let value = text(source, tokens, at);
        if value == "import" || value == "from" {
            if let Some((end, item)) = parse_import(source, tokens, at) {
                result.imports.push(item);
                at = end.saturating_add(1);
                continue;
            }
        }
        if value == "def" || value == "class" {
            let Some(name_at) = next_sig(tokens, at) else {
                at += 1;
                continue;
            };
            let Some(name) = ident(source, tokens, name_at) else {
                at += 1;
                continue;
            };
            let Some(colon) = find_colon(source, tokens, name_at) else {
                at += 1;
                continue;
            };
            let start_tok = decorated_start(source, tokens, at);
            let (body, end_tok) = suite_range(tokens, at, colon);
            let mut doc = String::new();
            if let Some((body_start, body_end)) = body {
                if body_start < body_end
                    && tokens
                        .get(body_start)
                        .is_some_and(|token| token.kind == TokenKind::String)
                {
                    doc = clean_doc(&string_value(tokens[body_start].text(source)));
                }
            }
            result.decls.push(Decl {
                name,
                kind: if value == "class" {
                    SymbolKind::Class
                } else {
                    SymbolKind::Function
                },
                start_tok,
                end_tok,
                sig_start: tokens[start_tok].start,
                sig_end: tokens[colon].start,
                body,
                callable: value == "def",
                parent_hint: None,
                doc,
            });
        }
        at += 1;
    }
    result
}

fn find_colon(source: &str, tokens: &[Token], mut at: usize) -> Option<usize> {
    let mut parens = 0usize;
    let mut brackets = 0usize;
    while at < tokens.len() {
        match text(source, tokens, at) {
            "(" => parens += 1,
            ")" => parens = parens.saturating_sub(1),
            "[" => brackets += 1,
            "]" => brackets = brackets.saturating_sub(1),
            ":" if parens == 0 && brackets == 0 => return Some(at),
            _ => {}
        }
        at += 1;
    }
    None
}

fn suite_range(tokens: &[Token], header: usize, colon: usize) -> (Option<(usize, usize)>, usize) {
    let fallback = tokens.len().saturating_sub(1);
    let mut first = colon.saturating_add(1);
    while first < tokens.len() && tokens[first].kind == TokenKind::Newline {
        first += 1;
    }
    if first >= tokens.len() {
        return (Some((fallback, tokens.len())), fallback);
    }
    if tokens[first].line == tokens[colon].line {
        let mut end = first;
        while end < tokens.len()
            && tokens[end].kind != TokenKind::Newline
            && tokens[end].line == tokens[first].line
        {
            end += 1;
        }
        return (
            Some((first, end)),
            end.saturating_sub(1).max(first).min(fallback),
        );
    }

    let header_indent = tokens[header].column;
    let body_indent = tokens[first].column;
    if body_indent <= header_indent {
        return (None, colon);
    }
    let mut end = first;
    let mut previous_line = tokens[first].line;
    while end < tokens.len() {
        let token = &tokens[end];
        if token.kind == TokenKind::Newline {
            end += 1;
            continue;
        }
        if token.line > previous_line && token.column < body_indent {
            break;
        }
        previous_line = token.end_line;
        end += 1;
    }
    let mut last = end.saturating_sub(1).min(fallback);
    while last > first && tokens[last].kind == TokenKind::Newline {
        last -= 1;
    }
    (Some((first, end)), last)
}

fn decorated_start(source: &str, tokens: &[Token], keyword: usize) -> usize {
    let mut start = keyword;
    if let Some(previous) = super::prev_sig(tokens, keyword) {
        if is(source, tokens, previous, "async") && tokens[previous].line == tokens[keyword].line {
            start = previous;
        }
    }
    loop {
        let line = tokens[start].line;
        if line <= 1 {
            break;
        }
        let Some(candidate) = (0..start).rev().find(|index| {
            tokens[*index].line == line.saturating_sub(1) && is(source, tokens, *index, "@")
        }) else {
            break;
        };
        start = candidate;
    }
    start
}

fn parse_import(
    source: &str,
    tokens: &[Token],
    start: usize,
) -> Option<(usize, crate::code::model::Import)> {
    let end = (start + 1..tokens.len())
        .find(|index| tokens[*index].kind == TokenKind::Newline)
        .unwrap_or(tokens.len());
    if is(source, tokens, start, "from") {
        let import_at = (start + 1..end).find(|index| is(source, tokens, *index, "import"))?;
        let mut path = String::new();
        for token in &tokens[start + 1..import_at] {
            if token.kind != TokenKind::Newline {
                path.push_str(token.text(source));
            }
        }
        let names = super::comma_names(source, tokens, import_at + 1, end)
            .into_iter()
            .filter(|name| name != "as")
            .collect();
        Some((end.saturating_sub(1), import(path, names)))
    } else {
        let mut path = String::new();
        let mut names = Vec::new();
        let mut cursor = start + 1;
        while cursor < end {
            if tokens[cursor].kind == TokenKind::Ident {
                let value = text(source, tokens, cursor);
                if value == "as" {
                    cursor += 2;
                    continue;
                }
                if path.is_empty() {
                    path.push_str(value);
                } else {
                    names.push(value.to_owned());
                }
            } else if is(source, tokens, cursor, ".") && names.is_empty() {
                path.push('.');
            }
            cursor += 1;
        }
        Some((end.saturating_sub(1), import(path, names)))
    }
}
