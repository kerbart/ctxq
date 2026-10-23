use super::{
    brace_depths, comma_names, declaration_start, docs_before, expression_body, find_body_or_end,
    ident, import, is, matching, next_sig, prev_sig, push_semicolon_fields, string_value, text,
    Decl, Recognized,
};
use crate::code::lex::{Token, TokenKind};
use crate::code::model::SymbolKind;
use crate::code::scope::ScopeTree;

const MODIFIERS: &[&str] = &[
    "export",
    "default",
    "declare",
    "abstract",
    "public",
    "private",
    "protected",
    "static",
    "readonly",
    "async",
    "override",
    "get",
    "set",
];

pub(crate) fn recognize(source: &str, tokens: &[Token], tree: &ScopeTree) -> Recognized {
    let mut result = Recognized::default();
    let mut at = 0usize;
    while at < tokens.len() {
        let value = text(source, tokens, at);
        if value == "import" {
            if let Some((end, item)) = parse_import(source, tokens, at) {
                result.imports.push(item);
                at = end.saturating_add(1);
                continue;
            }
        }
        let type_kind = match value {
            "class" => Some(SymbolKind::Class),
            "interface" => Some(SymbolKind::Interface),
            "enum" => Some(SymbolKind::Enum),
            "namespace" | "module" => Some(SymbolKind::Module),
            _ => None,
        };
        if let Some(kind) = type_kind {
            if let Some(name_at) = next_sig(tokens, at) {
                if let Some(name) = ident(source, tokens, name_at) {
                    let (body, end_tok, sig_end) =
                        find_body_or_end(source, tokens, tree, name_at + 1);
                    let start_tok = declaration_start(source, tokens, at, MODIFIERS);
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
            }
        } else if value == "type" {
            push_simple(source, tokens, tree, &mut result, at, SymbolKind::TypeAlias);
        } else if value == "function" {
            push_function(source, tokens, tree, &mut result, at);
        } else if matches!(value, "const" | "let" | "var") {
            push_variable(source, tokens, tree, &mut result, at, value == "const");
        }
        at += 1;
    }
    push_class_methods(source, tokens, tree, &mut result);
    push_semicolon_fields(source, tokens, &mut result);
    result
}

fn push_function(
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
    let start_tok = declaration_start(source, tokens, keyword, MODIFIERS);
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

fn push_simple(
    source: &str,
    tokens: &[Token],
    tree: &ScopeTree,
    result: &mut Recognized,
    keyword: usize,
    kind: SymbolKind,
) {
    let Some(name_at) = next_sig(tokens, keyword) else {
        return;
    };
    let Some(name) = ident(source, tokens, name_at) else {
        return;
    };
    let end_tok = statement_end(source, tokens, tree, name_at + 1);
    let start_tok = declaration_start(source, tokens, keyword, MODIFIERS);
    result.decls.push(Decl {
        name,
        kind,
        start_tok,
        end_tok,
        sig_start: tokens[start_tok].start,
        sig_end: tokens
            .get(end_tok)
            .map_or(source.len(), |token| token.start),
        body: None,
        callable: false,
        parent_hint: None,
        doc: docs_before(source, tokens, start_tok),
    });
}

fn push_variable(
    source: &str,
    tokens: &[Token],
    tree: &ScopeTree,
    result: &mut Recognized,
    keyword: usize,
    is_const: bool,
) {
    let Some(name_at) = next_sig(tokens, keyword) else {
        return;
    };
    let Some(name) = ident(source, tokens, name_at) else {
        return;
    };
    let end = statement_end(source, tokens, tree, name_at + 1);
    let arrow = (name_at + 1..=end.min(tokens.len().saturating_sub(1))).find(|index| {
        is(source, tokens, *index, "=")
            && next_sig(tokens, *index).is_some_and(|next| is(source, tokens, next, ">"))
    });
    let start_tok = declaration_start(source, tokens, keyword, MODIFIERS);
    if let Some(equals) = arrow {
        let greater = next_sig(tokens, equals).unwrap_or(equals);
        let after = next_sig(tokens, greater).unwrap_or(greater);
        let (body, end_tok, sig_end) = if is(source, tokens, after, "{") {
            let close = matching(tree, after, end);
            (
                Some((after.saturating_add(1), close)),
                close,
                tokens[after].start,
            )
        } else {
            let (body, expr_end) = expression_body(source, tokens, after);
            (body, expr_end, tokens[greater].end)
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
    } else {
        result.decls.push(Decl {
            name,
            kind: if is_const {
                SymbolKind::Const
            } else {
                SymbolKind::Variable
            },
            start_tok,
            end_tok: end,
            sig_start: tokens[start_tok].start,
            sig_end: tokens.get(end).map_or(source.len(), |token| token.start),
            body: None,
            callable: false,
            parent_hint: None,
            doc: docs_before(source, tokens, start_tok),
        });
    }
}

fn push_class_methods(source: &str, tokens: &[Token], tree: &ScopeTree, result: &mut Recognized) {
    let type_bodies: Vec<(usize, usize)> = result
        .decls
        .iter()
        .filter(|decl| {
            matches!(
                decl.kind,
                SymbolKind::Class | SymbolKind::Interface | SymbolKind::Module
            )
        })
        .filter_map(|decl| decl.body)
        .collect();
    let depths = brace_depths(source, tokens);
    for open in 0..tokens.len() {
        if !is(source, tokens, open, "(") {
            continue;
        }
        let Some((body_start, body_end)) = type_bodies
            .iter()
            .copied()
            .filter(|(start, end)| *start <= open && open < *end)
            .min_by_key(|(start, end)| end.saturating_sub(*start))
        else {
            continue;
        };
        if depths.get(open) != depths.get(body_start) {
            continue;
        }
        let Some(name_at) = method_name_before(source, tokens, open) else {
            continue;
        };
        let name = text(source, tokens, name_at);
        if is_method_keyword(name) {
            continue;
        }
        if let Some(previous) = prev_sig(tokens, name_at) {
            if matches!(text(source, tokens, previous), "." | "=" | "new" | "return") {
                continue;
            }
        }
        let close = matching(tree, open, open);
        if close == open {
            continue;
        }
        let mut delimiter = close.saturating_add(1);
        while delimiter < body_end {
            match text(source, tokens, delimiter) {
                "{" | ";" => break,
                "=" if next_sig(tokens, delimiter)
                    .is_some_and(|next| is(source, tokens, next, ">")) =>
                {
                    break
                }
                _ => delimiter += 1,
            }
        }
        if delimiter >= body_end {
            continue;
        }
        let start_tok = declaration_start(source, tokens, name_at, MODIFIERS);
        if result
            .decls
            .iter()
            .any(|decl| decl.start_tok == start_tok && decl.name == name)
        {
            continue;
        }
        let (body, end_tok, sig_end) = match text(source, tokens, delimiter) {
            "{" => {
                let end = matching(tree, delimiter, body_end);
                (
                    Some((delimiter.saturating_add(1), end)),
                    end,
                    tokens[delimiter].start,
                )
            }
            ";" => (None, delimiter, tokens[delimiter].start),
            _ => {
                let greater = next_sig(tokens, delimiter).unwrap_or(delimiter);
                let expr = next_sig(tokens, greater).unwrap_or(greater);
                let (body, end) = expression_body(source, tokens, expr);
                (body, end, tokens[greater].end)
            }
        };
        result.decls.push(Decl {
            name: name.to_owned(),
            kind: if name == "constructor" {
                SymbolKind::Constructor
            } else {
                SymbolKind::Method
            },
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
}

fn method_name_before(source: &str, tokens: &[Token], open: usize) -> Option<usize> {
    let mut at = prev_sig(tokens, open)?;
    if is(source, tokens, at, ">") {
        let mut depth = 1usize;
        while let Some(previous) = prev_sig(tokens, at) {
            at = previous;
            match text(source, tokens, at) {
                ">" => depth += 1,
                "<" => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        at = prev_sig(tokens, at)?;
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    (tokens.get(at)?.kind == TokenKind::Ident).then_some(at)
}

fn is_method_keyword(value: &str) -> bool {
    matches!(
        value,
        "if" | "for" | "while" | "switch" | "catch" | "with" | "function"
    )
}

fn statement_end(source: &str, tokens: &[Token], tree: &ScopeTree, mut at: usize) -> usize {
    let fallback = tokens.len().saturating_sub(1);
    while at < tokens.len() {
        if is(source, tokens, at, ";") {
            return at;
        }
        if matches!(text(source, tokens, at), "(" | "[" | "{") {
            if let Some(close) = tree.matching(at) {
                at = close.saturating_add(1);
                continue;
            }
        }
        at += 1;
    }
    fallback
}

fn parse_import(
    source: &str,
    tokens: &[Token],
    start: usize,
) -> Option<(usize, crate::code::model::Import)> {
    let end = (start + 1..tokens.len())
        .find(|index| is(source, tokens, *index, ";") || tokens[*index].kind == TokenKind::Newline)
        .unwrap_or(tokens.len().saturating_sub(1));
    let string_at = (start + 1..=end.min(tokens.len().saturating_sub(1)))
        .rev()
        .find(|index| tokens[*index].kind == TokenKind::String)?;
    let path = string_value(text(source, tokens, string_at));
    let names = comma_names(source, tokens, start + 1, string_at)
        .into_iter()
        .filter(|name| !matches!(name.as_str(), "from" | "import"))
        .collect();
    Some((end, import(path, names)))
}
