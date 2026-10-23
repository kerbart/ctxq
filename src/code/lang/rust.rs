use super::{
    brace_depths, declaration_start, docs_before, find_body_or_end, ident, import, is, matching,
    next_sig, text, Decl, Recognized,
};
use crate::code::lex::{Token, TokenKind};
use crate::code::model::SymbolKind;
use crate::code::scope::ScopeTree;

const MODIFIERS: &[&str] = &[
    "pub", "crate", "unsafe", "async", "const", "extern", "default", "auto",
];

pub(crate) fn recognize(source: &str, tokens: &[Token], tree: &ScopeTree) -> Recognized {
    let mut result = Recognized::default();
    let impls = impl_ranges(source, tokens, tree);
    let mut at = 0usize;
    while at < tokens.len() {
        let value = text(source, tokens, at);
        if value == "use" {
            if let Some((end, item)) = parse_use(source, tokens, at) {
                result.imports.push(item);
                at = end.saturating_add(1);
                continue;
            }
        }
        if value == "fn" {
            if let Some(name_at) = next_sig(tokens, at) {
                if let Some(name) = ident(source, tokens, name_at) {
                    let (body, end_tok, sig_end) =
                        find_body_or_end(source, tokens, tree, name_at + 1);
                    let start_tok = declaration_start(source, tokens, at, MODIFIERS);
                    let containing_impl = impls
                        .iter()
                        .filter(|range| range.start <= at && at < range.end)
                        .min_by_key(|range| range.end.saturating_sub(range.start));
                    result.decls.push(Decl {
                        name,
                        kind: if containing_impl.is_some() {
                            SymbolKind::Method
                        } else {
                            SymbolKind::Function
                        },
                        start_tok,
                        end_tok,
                        sig_start: tokens[start_tok].start,
                        sig_end,
                        body,
                        callable: true,
                        parent_hint: containing_impl.map(|range| range.target.clone()),
                        doc: docs_before(source, tokens, start_tok),
                    });
                    at = at.saturating_add(1);
                    continue;
                }
            }
        }

        let type_kind = match value {
            "struct" => Some(SymbolKind::Struct),
            "enum" => Some(SymbolKind::Enum),
            "trait" => Some(SymbolKind::Trait),
            "mod" => Some(SymbolKind::Module),
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
        } else if matches!(value, "type" | "const" | "static")
            && !(value == "static"
                && super::prev_sig(tokens, at)
                    .is_some_and(|previous| is(source, tokens, previous, "'")))
            && !(value == "const"
                && next_sig(tokens, at).is_some_and(|next| is(source, tokens, next, "fn")))
        {
            if let Some(name_at) = next_sig(tokens, at) {
                if let Some(name) = ident(source, tokens, name_at) {
                    let end_tok = semicolon_after(source, tokens, tree, name_at + 1);
                    let start_tok = declaration_start(source, tokens, at, MODIFIERS);
                    let sig_end = tokens
                        .get(end_tok)
                        .map_or(source.len(), |token| token.start);
                    result.decls.push(Decl {
                        name,
                        kind: match value {
                            "type" => SymbolKind::TypeAlias,
                            "const" => SymbolKind::Const,
                            _ => SymbolKind::Variable,
                        },
                        start_tok,
                        end_tok,
                        sig_start: tokens[start_tok].start,
                        sig_end,
                        body: None,
                        callable: false,
                        parent_hint: None,
                        doc: docs_before(source, tokens, start_tok),
                    });
                }
            }
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
        for colon in start..end.min(tokens.len()) {
            if !is(source, tokens, colon, ":") || depths.get(colon) != Some(&base_depth) {
                continue;
            }
            let Some(name_at) = super::prev_sig(tokens, colon) else {
                continue;
            };
            let Some(name) = ident(source, tokens, name_at) else {
                continue;
            };
            let mut field_start = name_at;
            if let Some(previous) = super::prev_sig(tokens, name_at) {
                if previous >= start && is(source, tokens, previous, "pub") {
                    field_start = previous;
                }
            }
            let mut field_end = colon;
            while field_end < end {
                if is(source, tokens, field_end, ",") && depths.get(field_end) == Some(&base_depth)
                {
                    break;
                }
                field_end += 1;
            }
            field_end = field_end.min(end.saturating_sub(1));
            result.decls.push(Decl {
                name,
                kind: SymbolKind::Field,
                start_tok: field_start,
                end_tok: field_end,
                sig_start: tokens[field_start].start,
                sig_end: tokens
                    .get(field_end)
                    .map_or(source.len(), |token| token.start),
                body: None,
                callable: false,
                parent_hint: None,
                doc: docs_before(source, tokens, field_start),
            });
        }
    }
}

#[derive(Debug)]
struct ImplRange {
    start: usize,
    end: usize,
    target: String,
}

fn impl_ranges(source: &str, tokens: &[Token], tree: &ScopeTree) -> Vec<ImplRange> {
    let mut ranges = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        if token.kind != TokenKind::Ident || token.text(source) != "impl" {
            continue;
        }
        let mut open = at + 1;
        while open < tokens.len() && !is(source, tokens, open, "{") {
            open += 1;
        }
        if open >= tokens.len() {
            continue;
        }
        let end = matching(tree, open, tokens.len().saturating_sub(1));
        let target = impl_target(source, tokens, at + 1, open);
        if !target.is_empty() {
            ranges.push(ImplRange {
                start: open.saturating_add(1),
                end,
                target,
            });
        }
    }
    ranges
}

fn impl_target(source: &str, tokens: &[Token], start: usize, end: usize) -> String {
    let for_at = (start..end).find(|index| is(source, tokens, *index, "for"));
    let mut at = for_at.map_or(start, |index| index.saturating_add(1));
    if for_at.is_none() && is(source, tokens, at, "<") {
        let mut depth = 0usize;
        while at < end {
            match text(source, tokens, at) {
                "<" => depth += 1,
                ">" => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        at += 1;
                        break;
                    }
                }
                _ => {}
            }
            at += 1;
        }
    }
    while at < end {
        if tokens[at].kind == TokenKind::Ident {
            let value = text(source, tokens, at);
            if !matches!(value, "where" | "unsafe") {
                return value.to_owned();
            }
        }
        at += 1;
    }
    String::new()
}

fn semicolon_after(source: &str, tokens: &[Token], tree: &ScopeTree, mut at: usize) -> usize {
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

fn parse_use(
    source: &str,
    tokens: &[Token],
    start: usize,
) -> Option<(usize, crate::code::model::Import)> {
    let end = (start + 1..tokens.len()).find(|index| is(source, tokens, *index, ";"))?;
    let mut compact = String::new();
    for token in &tokens[start + 1..end] {
        if token.kind != TokenKind::Newline {
            compact.push_str(token.text(source));
        }
    }
    let (path, names) = if let Some(brace) = compact.find("::{") {
        let path = compact[..brace].to_owned();
        let names = compact[brace + 3..]
            .trim_end_matches('}')
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty() && *name != "self" && *name != "*")
            .map(|name| name.split_whitespace().next().unwrap_or(name).to_owned())
            .collect();
        (path, names)
    } else {
        (compact, Vec::new())
    };
    (!path.is_empty()).then(|| (end, import(path, names)))
}
