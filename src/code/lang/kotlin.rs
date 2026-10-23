use super::{
    docs_before, expression_body, find_body_or_end, ident, import, is, matching, next_sig,
    prev_sig, text, Decl, Recognized,
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
                if let Some(item) = parse_import(source, tokens, at) {
                    result.imports.push(item);
                }
            }
            "class" | "interface" | "object" => push_type(source, tokens, tree, &mut result, at),
            "typealias" => push_alias(source, tokens, &mut result, at),
            "fun" => push_function(source, tokens, tree, &mut result, at),
            "constructor" => push_constructor(source, tokens, tree, &mut result, at),
            "val" | "var" => push_property(source, tokens, &mut result, at),
            _ => {}
        }
        at += 1;
    }
    result
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
    let (body, end_tok, sig_end) = find_body_or_end(source, tokens, tree, name_at + 1);
    let start_tok = annotated_start(source, tokens, keyword);
    let kind = match text(source, tokens, keyword) {
        "interface" => SymbolKind::Interface,
        "object" => SymbolKind::Class,
        _ => {
            if prev_sig(tokens, keyword)
                .is_some_and(|previous| is(source, tokens, previous, "enum"))
            {
                SymbolKind::Enum
            } else {
                SymbolKind::Class
            }
        }
    };
    result.decls.push(Decl {
        name,
        kind,
        start_tok,
        end_tok,
        sig_start: tokens[start_tok].start,
        sig_end,
        body,
        callable: false,
        parent_hint: None,
        doc: docs_before(source, tokens, start_tok),
    });
}

fn push_function(
    source: &str,
    tokens: &[Token],
    tree: &ScopeTree,
    result: &mut Recognized,
    keyword: usize,
) {
    let mut open = keyword + 1;
    while open < tokens.len() && !is(source, tokens, open, "(") {
        if is(source, tokens, open, "{") || is(source, tokens, open, ";") {
            return;
        }
        open += 1;
    }
    if open >= tokens.len() {
        return;
    }
    let Some(name_at) = prev_sig(tokens, open) else {
        return;
    };
    let Some(name) = ident(source, tokens, name_at) else {
        return;
    };
    let close = matching(tree, open, open);
    let mut delimiter = close.saturating_add(1);
    while delimiter < tokens.len() {
        if matches!(text(source, tokens, delimiter), "{" | "=" | ";")
            || tokens[delimiter].kind == TokenKind::Newline
        {
            break;
        }
        delimiter += 1;
    }
    let start_tok = annotated_start(source, tokens, keyword);
    let (body, end_tok, sig_end) = match text(source, tokens, delimiter) {
        "{" => {
            let end = matching(tree, delimiter, tokens.len().saturating_sub(1));
            (
                Some((delimiter.saturating_add(1), end)),
                end,
                tokens[delimiter].start,
            )
        }
        "=" => {
            let expression = next_sig(tokens, delimiter).unwrap_or(delimiter);
            let (body, end) = expression_body(source, tokens, expression);
            (body, end, tokens[delimiter].end)
        }
        _ => (
            None,
            delimiter.min(tokens.len().saturating_sub(1)),
            tokens
                .get(delimiter)
                .map_or(source.len(), |token| token.start),
        ),
    };
    result.decls.push(Decl {
        name,
        kind: SymbolKind::Function,
        start_tok,
        end_tok,
        sig_start: tokens[start_tok].start,
        sig_end,
        body,
        callable: true,
        parent_hint: None,
        doc: docs_before(source, tokens, start_tok),
    });
}

fn push_constructor(
    source: &str,
    tokens: &[Token],
    tree: &ScopeTree,
    result: &mut Recognized,
    keyword: usize,
) {
    let Some(open) = next_sig(tokens, keyword).filter(|index| is(source, tokens, *index, "("))
    else {
        return;
    };
    let close = matching(tree, open, open);
    let (body, end_tok, sig_end) = find_body_or_end(source, tokens, tree, close.saturating_add(1));
    let start_tok = annotated_start(source, tokens, keyword);
    result.decls.push(Decl {
        name: "constructor".to_owned(),
        kind: SymbolKind::Constructor,
        start_tok,
        end_tok,
        sig_start: tokens[start_tok].start,
        sig_end,
        body,
        callable: true,
        parent_hint: None,
        doc: docs_before(source, tokens, start_tok),
    });
}

fn push_alias(source: &str, tokens: &[Token], result: &mut Recognized, keyword: usize) {
    let Some(name_at) = next_sig(tokens, keyword) else {
        return;
    };
    let Some(name) = ident(source, tokens, name_at) else {
        return;
    };
    let end = line_end(tokens, name_at);
    result.decls.push(Decl {
        name,
        kind: SymbolKind::TypeAlias,
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

fn push_property(source: &str, tokens: &[Token], result: &mut Recognized, keyword: usize) {
    let Some(name_at) = next_sig(tokens, keyword) else {
        return;
    };
    let Some(name) = ident(source, tokens, name_at) else {
        return;
    };
    let end = line_end(tokens, name_at);
    result.decls.push(Decl {
        name,
        kind: if is(source, tokens, keyword, "val") {
            SymbolKind::Const
        } else {
            SymbolKind::Variable
        },
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

fn annotated_start(source: &str, tokens: &[Token], keyword: usize) -> usize {
    let mut start = keyword;
    let mut cursor = keyword;
    while let Some(previous) = prev_sig(tokens, cursor) {
        let value = text(source, tokens, previous);
        if matches!(
            value,
            "public"
                | "private"
                | "protected"
                | "internal"
                | "open"
                | "final"
                | "abstract"
                | "override"
                | "suspend"
                | "data"
                | "sealed"
                | "enum"
        ) {
            start = previous;
            cursor = previous;
        } else {
            break;
        }
    }
    start
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

fn parse_import(
    source: &str,
    tokens: &[Token],
    start: usize,
) -> Option<crate::code::model::Import> {
    let end = line_end(tokens, start);
    let mut path = String::new();
    for index in start + 1..=end {
        let value = text(source, tokens, index);
        if value == "as" {
            break;
        }
        if tokens[index].kind == TokenKind::Ident || value == "." {
            path.push_str(value);
        }
    }
    (!path.is_empty()).then(|| import(path, Vec::new()))
}
