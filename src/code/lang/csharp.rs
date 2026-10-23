use super::{
    brace_depths, declaration_start, docs_before, expression_body, find_body_or_end, ident, import,
    is, matching, next_sig, prev_sig, push_semicolon_fields, text, Decl, Recognized,
};
use crate::code::lex::{Token, TokenKind};
use crate::code::model::SymbolKind;
use crate::code::scope::ScopeTree;

const MODIFIERS: &[&str] = &[
    "public",
    "protected",
    "private",
    "internal",
    "static",
    "abstract",
    "sealed",
    "partial",
    "readonly",
    "unsafe",
    "new",
    "virtual",
    "override",
    "async",
    "extern",
    "required",
];

pub(crate) fn recognize(source: &str, tokens: &[Token], tree: &ScopeTree) -> Recognized {
    let mut result = Recognized::default();
    let mut file_namespace: Option<String> = None;
    let mut at = 0usize;
    while at < tokens.len() {
        let value = text(source, tokens, at);
        if value == "using" {
            if let Some(item) = parse_using(source, tokens, at) {
                result.imports.push(item);
            }
        } else if value == "namespace" {
            if let Some((decl, file_scoped)) = parse_namespace(source, tokens, tree, at) {
                if file_scoped {
                    file_namespace = Some(decl.name.clone());
                }
                result.decls.push(decl);
            }
        } else if matches!(value, "class" | "record" | "struct" | "interface" | "enum") {
            if let Some(mut decl) = parse_type(source, tokens, tree, at) {
                if decl.parent_hint.is_none() && file_namespace.is_some() {
                    decl.parent_hint.clone_from(&file_namespace);
                }
                result.decls.push(decl);
            }
        }
        at += 1;
    }
    push_methods(source, tokens, tree, &mut result);
    push_semicolon_fields(source, tokens, &mut result);
    result
}

fn parse_type(source: &str, tokens: &[Token], tree: &ScopeTree, keyword: usize) -> Option<Decl> {
    let mut name_at = next_sig(tokens, keyword)?;
    if is(source, tokens, keyword, "record")
        && matches!(text(source, tokens, name_at), "class" | "struct")
    {
        name_at = next_sig(tokens, name_at)?;
    }
    let name = ident(source, tokens, name_at)?;
    let kind = match text(source, tokens, keyword) {
        "struct" => SymbolKind::Struct,
        "interface" => SymbolKind::Interface,
        "enum" => SymbolKind::Enum,
        _ => SymbolKind::Class,
    };
    let (body, end_tok, sig_end) = find_body_or_end(source, tokens, tree, name_at + 1);
    let start_tok = declaration_start(source, tokens, keyword, MODIFIERS);
    Some(Decl {
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
    })
}

fn parse_namespace(
    source: &str,
    tokens: &[Token],
    tree: &ScopeTree,
    keyword: usize,
) -> Option<(Decl, bool)> {
    let mut at = next_sig(tokens, keyword)?;
    let mut name = String::new();
    while at < tokens.len() {
        let value = text(source, tokens, at);
        if matches!(value, "{" | ";") {
            break;
        }
        if tokens[at].kind == TokenKind::Ident || value == "." {
            name.push_str(value);
        }
        at += 1;
    }
    if name.is_empty() || at >= tokens.len() {
        return None;
    }
    let file_scoped = is(source, tokens, at, ";");
    let (body, end_tok) = if file_scoped {
        (None, at)
    } else {
        let close = matching(tree, at, tokens.len().saturating_sub(1));
        (Some((at.saturating_add(1), close)), close)
    };
    Some((
        Decl {
            name,
            kind: SymbolKind::Module,
            start_tok: keyword,
            end_tok,
            sig_start: tokens[keyword].start,
            sig_end: tokens[at].start,
            body,
            callable: false,
            parent_hint: None,
            doc: docs_before(source, tokens, keyword),
        },
        file_scoped,
    ))
}

fn push_methods(source: &str, tokens: &[Token], tree: &ScopeTree, result: &mut Recognized) {
    let types: Vec<(usize, usize, String)> = result
        .decls
        .iter()
        .filter(|decl| {
            matches!(
                decl.kind,
                SymbolKind::Class | SymbolKind::Struct | SymbolKind::Interface
            )
        })
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
        let start_tok = member_start(source, tokens, *body_start, name_at);
        if contains_arrow(source, tokens, start_tok, name_at) {
            continue;
        }
        let close = matching(tree, open, open);
        if close == open {
            continue;
        }
        let mut delimiter = close.saturating_add(1);
        while delimiter < *body_end {
            if matches!(text(source, tokens, delimiter), "{" | ";")
                || (is(source, tokens, delimiter, "=")
                    && next_sig(tokens, delimiter)
                        .is_some_and(|next| is(source, tokens, next, ">")))
            {
                break;
            }
            delimiter += 1;
        }
        if delimiter >= *body_end {
            continue;
        }
        let name = text(source, tokens, name_at).to_owned();
        let (body, end_tok, sig_end) = if is(source, tokens, delimiter, "{") {
            let end = matching(tree, delimiter, *body_end);
            (
                Some((delimiter.saturating_add(1), end)),
                end,
                tokens[delimiter].start,
            )
        } else if is(source, tokens, delimiter, ";") {
            (None, delimiter, tokens[delimiter].start)
        } else {
            let greater = next_sig(tokens, delimiter).unwrap_or(delimiter);
            let expression = next_sig(tokens, greater).unwrap_or(greater);
            let (body, end) = expression_body(source, tokens, expression);
            (body, end, tokens[greater].end)
        };
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

fn contains_arrow(source: &str, tokens: &[Token], start: usize, end: usize) -> bool {
    (start..end).any(|index| {
        is(source, tokens, index, "=")
            && next_sig(tokens, index).is_some_and(|next| is(source, tokens, next, ">"))
    })
}

fn is_keyword(value: &str) -> bool {
    matches!(
        value,
        "if" | "for" | "foreach" | "while" | "switch" | "catch" | "lock" | "using" | "nameof"
    )
}

fn parse_using(source: &str, tokens: &[Token], start: usize) -> Option<crate::code::model::Import> {
    let end = (start + 1..tokens.len()).find(|index| is(source, tokens, *index, ";"))?;
    let equals = (start + 1..end).find(|index| is(source, tokens, *index, "="));
    let path_start = equals.map_or(start + 1, |index| index + 1);
    let mut path = String::new();
    for token in &tokens[path_start..end] {
        if (token.kind == TokenKind::Ident || token.text(source) == ".")
            && token.text(source) != "static"
        {
            path.push_str(token.text(source));
        }
    }
    let names = equals
        .and_then(|_| ident(source, tokens, start + 1))
        .into_iter()
        .collect();
    (!path.is_empty()).then(|| import(path, names))
}
