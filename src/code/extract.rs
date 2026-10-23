//! Pure entry point of the home-grown extractor.

use super::lang;
use super::lex;
use super::model::{FileOutline, Lang};
use super::scope::ScopeTree;

/// Pure and total: never panics, partial result on invalid input.
pub fn extract(lang: Lang, source: &str) -> FileOutline {
    let lexed = lex::lex(lang, source);
    let scopes = ScopeTree::build(lang, source, &lexed.tokens, lexed.line_count);
    let recognized = match lang {
        Lang::Rust => lang::rust::recognize(source, &lexed.tokens, &scopes),
        Lang::TypeScript | Lang::JavaScript => {
            lang::typescript::recognize(source, &lexed.tokens, &scopes)
        }
        Lang::Python => lang::python::recognize(source, &lexed.tokens, &scopes),
        Lang::Go => lang::go::recognize(source, &lexed.tokens, &scopes),
        Lang::Java => lang::java::recognize(source, &lexed.tokens, &scopes),
        Lang::Kotlin => lang::kotlin::recognize(source, &lexed.tokens, &scopes),
        Lang::CSharp => lang::csharp::recognize(source, &lexed.tokens, &scopes),
    };
    lang::finish(lang, source, &lexed.tokens, recognized)
}
