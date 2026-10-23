use super::{
    brace_depths, docs_before, find_body_or_end, ident, import, is, matching, next_sig,
    string_value, text, Decl, Recognized,
};
use crate::code::lex::{Token, TokenKind};
use crate::code::model::SymbolKind;
use crate::code::scope::ScopeTree;

pub(crate) fn recognize(source: &str, tokens: &[Token], tree: &ScopeTree) -> Recognized {
    let mut result = Recognized::default();
    let mut at = 0usize;
    while at < tokens.len() {
        match text(source, tokens, at) {
            "import" => {
                if let Some((end, imports)) = parse_imports(source, tokens, tree, at) {
                    result.imports.extend(imports);
                    at = end;
                }
            }
            "func" => push_func(source, tokens, tree, &mut result, at),
            "type" => push_type(source, tokens, tree, &mut result, at),
            "const" | "var" => push_value(source, tokens, tree, &mut result, at),
            _ => {}
        }
        at += 1;
    }
    push_struct_fields(source, tokens, &mut result);
    result
}

fn push_struct_fields(source: &str, tokens: &[Token], result: &mut Recognized) {
    let bodies: Vec<(usize, usize)> = result
        .decls
        .iter()
        .filter(|decl| decl.kind == SymbolKind::Struct)
        .filter_map(|decl| decl.body)
        .collect();
    if bodies.is_empty() {
        return;
    }
    let depths = brace_depths(source, tokens);
    for (start, end) in bodies {
        let Some(base_depth) = depths.get(start).copied() else {
            continue;
        };
        let mut at = start;
        while at < end.min(tokens.len()) {
            if tokens[at].kind != TokenKind::Ident || depths.get(at) != Some(&base_depth) {
                at += 1;
                continue;
            }
            if super::prev_sig(tokens, at).is_some_and(|previous| {
                previous >= start && tokens[previous].end_line == tokens[at].line
            }) {
                at += 1;
                continue;
            }
            let Some(name) = ident(source, tokens, at) else {
                at += 1;
                continue;
            };
            let mut line_end = at;
            while line_end + 1 < end
                && tokens[line_end + 1].kind != TokenKind::Newline
                && tokens[line_end + 1].line == tokens[at].line
            {
                line_end += 1;
            }
            result.decls.push(Decl {
                name,
                kind: SymbolKind::Field,
                start_tok: at,
                end_tok: line_end,
                sig_start: tokens[at].start,
                sig_end: tokens[line_end].end,
                body: None,
                callable: false,
                parent_hint: None,
                doc: docs_before(source, tokens, at),
            });
            at = line_end.saturating_add(1);
        }
    }
}

fn push_func(
    source: &str,
    tokens: &[Token],
    tree: &ScopeTree,
    result: &mut Recognized,
    keyword: usize,
) {
    let Some(first) = next_sig(tokens, keyword) else {
        return;
    };
    let (name_at, receiver) = if is(source, tokens, first, "(") {
        let close = matching(tree, first, first);
        if close == first {
            return;
        }
        let ids: Vec<String> = (first + 1..close)
            .filter_map(|index| ident(source, tokens, index))
            .collect();
        let receiver = if ids.len() >= 2 {
            ids.get(1).cloned()
        } else {
            ids.first().cloned()
        };
        let Some(name_at) = next_sig(tokens, close) else {
            return;
        };
        (name_at, receiver)
    } else {
        (first, None)
    };
    let Some(name) = ident(source, tokens, name_at) else {
        return;
    };
    let (body, end_tok, sig_end) = find_body_or_end(source, tokens, tree, name_at + 1);
    result.decls.push(Decl {
        name,
        kind: if receiver.is_some() {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        },
        start_tok: keyword,
        end_tok,
        sig_start: tokens[keyword].start,
        sig_end,
        body,
        callable: true,
        parent_hint: receiver,
        doc: docs_before(source, tokens, keyword),
    });
}

fn push_type(
    source: &str,
    tokens: &[Token],
    tree: &ScopeTree,
    result: &mut Recognized,
    keyword: usize,
) {
    let Some(name_at) = next_sig(tokens, keyword) else {
        return;
    };
    let Some(name) = ident(source, tokens, name_at) else {
        return;
    };
    let mut cursor = name_at + 1;
    while cursor < tokens.len() && tokens[cursor].kind == TokenKind::Newline {
        cursor += 1;
    }
    if is(source, tokens, cursor, "[") {
        cursor = matching(tree, cursor, cursor).saturating_add(1);
        while cursor < tokens.len() && tokens[cursor].kind == TokenKind::Newline {
            cursor += 1;
        }
    }
    let kind = match text(source, tokens, cursor) {
        "struct" => SymbolKind::Struct,
        "interface" => SymbolKind::Interface,
        _ => SymbolKind::TypeAlias,
    };
    let (body, end_tok, sig_end) = if kind == SymbolKind::TypeAlias {
        let end = line_end(tokens, cursor);
        (
            None,
            end,
            tokens.get(end).map_or(source.len(), |token| token.end),
        )
    } else {
        find_body_or_end(source, tokens, tree, cursor)
    };
    result.decls.push(Decl {
        name,
        kind,
        start_tok: keyword,
        end_tok,
        sig_start: tokens[keyword].start,
        sig_end,
        body,
        callable: false,
        parent_hint: None,
        doc: docs_before(source, tokens, keyword),
    });
}

fn push_value(
    source: &str,
    tokens: &[Token],
    tree: &ScopeTree,
    result: &mut Recognized,
    keyword: usize,
) {
    let Some(name_at) = next_sig(tokens, keyword) else {
        return;
    };
    if is(source, tokens, name_at, "(") {
        return;
    }
    let Some(name) = ident(source, tokens, name_at) else {
        return;
    };
    let end = line_end(tokens, name_at);
    let kind = if is(source, tokens, keyword, "const") {
        SymbolKind::Const
    } else {
        SymbolKind::Variable
    };
    let _ = tree;
    result.decls.push(Decl {
        name,
        kind,
        start_tok: keyword,
        end_tok: end,
        sig_start: tokens[keyword].start,
        sig_end: tokens.get(end).map_or(source.len(), |token| token.end),
        body: None,
        callable: false,
        parent_hint: None,
        doc: docs_before(source, tokens, keyword),
    });
}

fn line_end(tokens: &[Token], start: usize) -> usize {
    let line = tokens.get(start).map_or(1, |token| token.line);
    (start..tokens.len())
        .take_while(|index| {
            tokens[*index].line == line && tokens[*index].kind != TokenKind::Newline
        })
        .last()
        .unwrap_or(start)
}

fn parse_imports(
    source: &str,
    tokens: &[Token],
    tree: &ScopeTree,
    keyword: usize,
) -> Option<(usize, Vec<crate::code::model::Import>)> {
    let first = next_sig(tokens, keyword)?;
    if tokens[first].kind == TokenKind::String {
        return Some((
            line_end(tokens, first),
            vec![import(
                string_value(text(source, tokens, first)),
                Vec::new(),
            )],
        ));
    }
    if !is(source, tokens, first, "(") {
        return None;
    }
    let close = matching(tree, first, tokens.len().saturating_sub(1));
    let imports = (first + 1..close)
        .filter(|index| tokens[*index].kind == TokenKind::String)
        .map(|index| import(string_value(text(source, tokens, index)), Vec::new()))
        .collect();
    Some((close, imports))
}
