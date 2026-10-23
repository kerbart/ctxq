//! Recoverable delimiter and indentation scopes.

use super::lex::{Token, TokenKind};
use super::model::Lang;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Root,
    Brace,
    Paren,
    Bracket,
    Indent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub kind: ScopeKind,
    pub parent: Option<usize>,
    pub start_token: usize,
    pub end_token: usize,
    pub start_line: u32,
    pub end_line: u32,
    pub children: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct ScopeTree {
    pub scopes: Vec<Scope>,
    /// Opening and closing delimiter token indexes point to one another.
    pub pairs: Vec<Option<usize>>,
}

impl ScopeTree {
    pub fn build(lang: Lang, source: &str, tokens: &[Token], line_count: u32) -> Self {
        let last_token = tokens.len().saturating_sub(1);
        let mut scopes = vec![Scope {
            kind: ScopeKind::Root,
            parent: None,
            start_token: 0,
            end_token: last_token,
            start_line: 1,
            end_line: line_count,
            children: Vec::new(),
        }];
        let mut pairs = vec![None; tokens.len()];
        let mut stack: Vec<(ScopeKind, usize, usize)> = Vec::new();

        for (index, token) in tokens.iter().enumerate() {
            let kind = match token.text(source) {
                "{" => Some(ScopeKind::Brace),
                "(" => Some(ScopeKind::Paren),
                "[" => Some(ScopeKind::Bracket),
                "}" => Some(ScopeKind::Brace),
                ")" => Some(ScopeKind::Paren),
                "]" => Some(ScopeKind::Bracket),
                _ => None,
            };
            let Some(kind) = kind else { continue };
            let opening = matches!(token.text(source), "{" | "(" | "[");
            if opening {
                let parent = stack.last().map_or(0, |(_, _, scope)| *scope);
                let scope_index = scopes.len();
                scopes.push(Scope {
                    kind,
                    parent: Some(parent),
                    start_token: index,
                    end_token: last_token,
                    start_line: token.line,
                    end_line: line_count,
                    children: Vec::new(),
                });
                scopes[parent].children.push(scope_index);
                stack.push((kind, index, scope_index));
                continue;
            }

            while stack.last().is_some_and(|(open, _, _)| *open != kind) {
                if let Some((_, _, abandoned)) = stack.pop() {
                    scopes[abandoned].end_token = index.saturating_sub(1);
                    scopes[abandoned].end_line = token.line;
                }
            }
            if let Some((_, open_token, scope_index)) = stack.pop() {
                pairs[open_token] = Some(index);
                pairs[index] = Some(open_token);
                scopes[scope_index].end_token = index;
                scopes[scope_index].end_line = token.line;
            }
        }
        for (_, _, scope_index) in stack {
            scopes[scope_index].end_token = last_token;
            scopes[scope_index].end_line = line_count;
        }

        if lang == Lang::Python {
            add_python_indents(source, tokens, line_count, &mut scopes);
        }
        Self { scopes, pairs }
    }

    pub fn matching(&self, token: usize) -> Option<usize> {
        self.pairs.get(token).copied().flatten()
    }
}

fn add_python_indents(source: &str, tokens: &[Token], line_count: u32, scopes: &mut Vec<Scope>) {
    let last_token = tokens.len().saturating_sub(1);
    let mut stack: Vec<(u32, usize)> = vec![(0, 0)];
    let mut delimiter_depth = 0usize;
    let mut at_line_start = true;

    for (index, token) in tokens.iter().enumerate() {
        if token.kind == TokenKind::Newline {
            at_line_start = true;
            continue;
        }
        if matches!(token.kind, TokenKind::DocComment) {
            continue;
        }
        if !at_line_start {
            delimiter_depth = adjusted_depth(source, delimiter_depth, token);
            continue;
        }
        at_line_start = false;
        if delimiter_depth == 0 {
            let indent = token.column.saturating_sub(1);
            while stack.len() > 1 && stack.last().is_some_and(|(level, _)| *level > indent) {
                if let Some((_, scope_index)) = stack.pop() {
                    scopes[scope_index].end_token = index.saturating_sub(1);
                    scopes[scope_index].end_line = token.line.saturating_sub(1);
                }
            }
            if indent > stack.last().map_or(0, |(level, _)| *level) {
                let parent = stack.last().map_or(0, |(_, scope)| *scope);
                let scope_index = scopes.len();
                scopes.push(Scope {
                    kind: ScopeKind::Indent,
                    parent: Some(parent),
                    start_token: index,
                    end_token: last_token,
                    start_line: token.line,
                    end_line: line_count,
                    children: Vec::new(),
                });
                scopes[parent].children.push(scope_index);
                stack.push((indent, scope_index));
            }
        }
        delimiter_depth = adjusted_depth(source, delimiter_depth, token);
    }
}

fn adjusted_depth(source: &str, depth: usize, token: &Token) -> usize {
    match token.text(source) {
        "(" | "[" | "{" => depth.saturating_add(1),
        ")" | "]" | "}" => depth.saturating_sub(1),
        _ => depth,
    }
}
