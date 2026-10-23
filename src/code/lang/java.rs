use super::{
    brace_depths, declaration_start, docs_before, find_body_or_end, ident, import, is, matching,
    next_sig, prev_sig, push_semicolon_fields, text, Decl, Recognized,
};
use crate::code::lex::{Token, TokenKind};
use crate::code::model::SymbolKind;
use crate::code::scope::ScopeTree;

const MODIFIERS: &[&str] = &[
    "public",
    "protected",
    "private",
    "static",
    "abstract",
    "final",
    "sealed",
    "non",
    "strictfp",
    "synchronized",
    "native",
    "default",
    "transient",
    "volatile",
];

pub(crate) fn recognize(source: &str, tokens: &[Token], tree: &ScopeTree) -> Recognized {
    let mut result = Recognized::default();
    for at in 0..tokens.len() {
        let value = text(source, tokens, at);
        if value == "import" {
            if let Some(item) = parse_import(source, tokens, at) {
                result.imports.push(item);
            }
            continue;
        }
        let kind = match value {
            "class" | "record" => Some(SymbolKind::Class),
            "interface" => Some(SymbolKind::Interface),
            "enum" => Some(SymbolKind::Enum),
            _ => None,
        };
        if let Some(kind) = kind {
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
        }
    }
    push_methods(source, tokens, tree, &mut result);
    push_semicolon_fields(source, tokens, &mut result);
    result
}

fn push_methods(source: &str, tokens: &[Token], tree: &ScopeTree, result: &mut Recognized) {
    let types: Vec<(usize, usize, String)> = result
        .decls
        .iter()
        .filter_map(|decl| {
            decl.body
                .map(|(start, end)| (start, end, decl.name.clone()))
        })
        .collect();
    let depths = brace_depths(source, tokens);
    for open in 0..tokens.len() {
        if !is(source, tokens, open, "(") {
            continue;
        }
        let Some((body_start, body_end, type_name)) = types
            .iter()
            .filter(|(start, end, _)| *start <= open && open < *end)
            .min_by_key(|(start, end, _)| end.saturating_sub(*start))
        else {
            continue;
        };
        if depths.get(open) != depths.get(*body_start) {
            continue;
        }
        let Some(name_at) = prev_sig(tokens, open) else {
            continue;
        };
        if tokens[name_at].kind != TokenKind::Ident || is_keyword(text(source, tokens, name_at)) {
            continue;
        }
        if prev_sig(tokens, name_at).is_some_and(|previous| {
            matches!(
                text(source, tokens, previous),
                "." | "=" | "new" | "return" | "@"
            )
        }) {
            continue;
        }
        let close = matching(tree, open, open);
        if close == open {
            continue;
        }
        let mut delimiter = close.saturating_add(1);
        while delimiter < *body_end && !matches!(text(source, tokens, delimiter), "{" | ";") {
            delimiter += 1;
        }
        if delimiter >= *body_end {
            continue;
        }
        let start_tok = member_start(source, tokens, *body_start, name_at);
        if result
            .decls
            .iter()
            .any(|decl| decl.start_tok == start_tok && decl.name == text(source, tokens, name_at))
        {
            continue;
        }
        let (body, end_tok, sig_end) = if is(source, tokens, delimiter, "{") {
            let end = matching(tree, delimiter, *body_end);
            (
                Some((delimiter.saturating_add(1), end)),
                end,
                tokens[delimiter].start,
            )
        } else {
            (None, delimiter, tokens[delimiter].start)
        };
        let name = text(source, tokens, name_at).to_owned();
        result.decls.push(Decl {
            kind: if name == *type_name {
                SymbolKind::Constructor
            } else {
                SymbolKind::Method
            },
            name,
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

fn member_start(source: &str, tokens: &[Token], lower: usize, name: usize) -> usize {
    let mut start = name;
    let mut cursor = name;
    while let Some(previous) = prev_sig(tokens, cursor) {
        if previous < lower || matches!(text(source, tokens, previous), ";" | "{" | "}") {
            break;
        }
        start = previous;
        cursor = previous;
    }
    start
}

fn is_keyword(value: &str) -> bool {
    matches!(
        value,
        "if" | "for" | "while" | "switch" | "catch" | "try" | "synchronized" | "super"
    )
}

fn parse_import(
    source: &str,
    tokens: &[Token],
    start: usize,
) -> Option<crate::code::model::Import> {
    let end = (start + 1..tokens.len()).find(|index| is(source, tokens, *index, ";"))?;
    let mut path = String::new();
    for token in &tokens[start + 1..end] {
        if (token.kind == TokenKind::Ident || matches!(token.text(source), "." | "*"))
            && token.text(source) != "static"
        {
            path.push_str(token.text(source));
        }
    }
    (!path.is_empty()).then(|| import(path, Vec::new()))
}
