//! Lexical side of document search:
//! tokenisation with compound identifiers, **global** BM25 with absolute
//! normalisation, routing of the lexical weight.
//!
//! Pure cosine goes astray on exact identifiers (a port, a container name, a
//! domain), which is precisely the dominant query profile of an infra
//! knowledge base. BM25 fixes that, provided it is computed over the WHOLE
//! corpus: an identifier missing from the dense pool would otherwise be
//! unfindable.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use crate::core::mdchunk::CRUMB_SEP;
use crate::core::parser::DocChunk;

pub const K1: f32 = 1.5;
pub const B: f32 = 0.75;

/// Captures compound identifiers (`my-app.example.com`) whole.
static TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[a-z0-9]+(?:[._-][a-z0-9]+)*").unwrap());
static ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\d|[a-z0-9][._@-][a-z0-9]|\.service\b").unwrap());
/// Words that signal an identifier lookup: the ANSWER is one, even when the
/// question contains none ("port de l'appli budget"). Common infrastructure
/// vocabulary, French and English (do not translate: it drives routing);
/// replaceable via `[search] lookup_terms`.
pub const DEFAULT_LOOKUP_TERMS: &[&str] = &[
    "port",
    "ports",
    "ip",
    "pin",
    "adresse",
    "adresses",
    "address",
    "addresses",
    "domaine",
    "domaines",
    "domain",
    "domains",
    "sous-domaine",
    "sous-domaines",
    "subdomain",
    "hostname",
    "hostnames",
    "conteneur",
    "conteneurs",
    "container",
    "containers",
    "unité",
    "unités",
    "unite",
    "unites",
    "unit",
    "units",
    "systemd",
    "url",
    "chemin",
    "path",
];

/// Tokenises keeping the WHOLE compound identifier *and* its sub-parts:
/// `app.example.com` → `app.example.com`, `app`, `example`, `com`.
pub fn tokenize(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut out = Vec::new();
    for m in TOKEN_RE.find_iter(&lower) {
        let tok = m.as_str();
        if tok.chars().count() >= 2 {
            out.push(tok.to_string());
        }
        if tok.contains(['.', '_', '-']) {
            out.extend(
                tok.split(['.', '_', '-'])
                    .filter(|p| p.chars().count() >= 2)
                    .map(str::to_string),
            );
        }
    }
    out
}

/// Routing: does the query carry (or call for) an exact identifier?
pub struct IdRouter {
    lookup: Option<Regex>,
}

impl IdRouter {
    pub fn new(terms: Option<&[String]>) -> anyhow::Result<Self> {
        let owned: Vec<String>;
        let terms: Vec<&str> = match terms {
            Some(t) => {
                owned = t.to_vec();
                owned.iter().map(String::as_str).collect()
            }
            None => DEFAULT_LOOKUP_TERMS.to_vec(),
        };
        let lookup = if terms.is_empty() {
            None
        } else {
            let alt = terms
                .iter()
                .map(|t| regex::escape(t))
                .collect::<Vec<_>>()
                .join("|");
            Some(Regex::new(&format!(r"(?i)\b({alt})\b"))?)
        };
        Ok(Self { lookup })
    }

    pub fn is_identifier_query(&self, query: &str) -> bool {
        ID_RE.is_match(query) || self.lookup.as_ref().is_some_and(|r| r.is_match(query))
    }
}

/// Routing with the default vocabulary.
pub fn is_identifier_query(query: &str) -> bool {
    static DEFAULT: LazyLock<IdRouter> = LazyLock::new(|| IdRouter::new(None).unwrap());
    DEFAULT.is_identifier_query(query)
}

/// Text seen by BM25: source path + breadcrumb + body. The path carries the
/// project name, which is often absent from the body itself.
pub fn lexical_text(ch: &DocChunk) -> String {
    format!(
        "{}\n{}\n{}",
        ch.source_path,
        ch.hierarchy.join(CRUMB_SEP),
        ch.content
    )
}

/// Lexical statistics + inverted index of the whole corpus.
pub struct Corpus {
    df: HashMap<String, u32>,
    dls: Vec<f32>,
    avgdl: f32,
    n: usize,
    postings: HashMap<String, (Vec<u32>, Vec<f32>)>,
}

impl Corpus {
    pub fn new(chunks: &[DocChunk]) -> Self {
        let mut postings: HashMap<String, (Vec<u32>, Vec<f32>)> = HashMap::new();
        let mut dls = Vec::with_capacity(chunks.len());
        for (i, ch) in chunks.iter().enumerate() {
            let toks = tokenize(&lexical_text(ch));
            dls.push(toks.len().max(1) as f32);
            let mut tf: HashMap<String, f32> = HashMap::new();
            for t in toks {
                *tf.entry(t).or_insert(0.0) += 1.0;
            }
            for (t, f) in tf {
                let e = postings.entry(t).or_default();
                e.0.push(i as u32);
                e.1.push(f);
            }
        }
        let df = postings
            .iter()
            .map(|(t, (ix, _))| (t.clone(), ix.len() as u32))
            .collect();
        let n = dls.len();
        let avgdl = if n == 0 {
            1.0
        } else {
            dls.iter().sum::<f32>() / n as f32
        };
        Self {
            df,
            dls,
            avgdl,
            n,
            postings,
        }
    }

    fn idf(&self, df: u32) -> f32 {
        (1.0 + (self.n as f32 - df as f32 + 0.5) / (df as f32 + 0.5)).ln()
    }

    fn term(&self, f: f32, df: u32, i: usize) -> f32 {
        self.idf(df) * (f * (K1 + 1.0)) / (f + K1 * (1.0 - B + B * self.dls[i] / self.avgdl))
    }

    /// Maximum BM25 score the query can reach (saturated tf, short
    /// document). ABSOLUTE denominator: normalising by the pool maximum gave
    /// the full bonus to a mediocre match, which demoted good top semantic
    /// results.
    pub fn bm25_max(&self, qtoks: &HashSet<String>) -> f32 {
        qtoks
            .iter()
            .filter_map(|t| self.df.get(t))
            .map(|&df| self.idf(df) * (K1 + 1.0))
            .sum()
    }

    /// BM25 of the pool candidates only (vector aligned with `pool`). The idf
    /// stays that of the whole corpus: the score equals an exhaustive computation.
    pub fn bm25_pool(
        &self,
        qtoks: &HashSet<String>,
        chunks: &[DocChunk],
        pool: &[usize],
    ) -> Vec<f32> {
        pool.iter()
            .map(|&i| {
                let mut tf: HashMap<String, f32> = HashMap::new();
                for t in tokenize(&lexical_text(&chunks[i])) {
                    if qtoks.contains(&t) {
                        *tf.entry(t).or_insert(0.0) += 1.0;
                    }
                }
                tf.iter()
                    .filter_map(|(t, &f)| Some(self.term(f, *self.df.get(t)?, i)))
                    .sum()
            })
            .collect()
    }

    /// Best BM25 chunks over the WHOLE corpus, via the inverted index.
    pub fn bm25_global_top(&self, qtoks: &HashSet<String>, n_top: usize) -> Vec<usize> {
        let mut scores = vec![0f32; self.n];
        for t in qtoks {
            let Some((ix, tfs)) = self.postings.get(t) else {
                continue;
            };
            let df = ix.len() as u32;
            for (&i, &f) in ix.iter().zip(tfs) {
                scores[i as usize] += self.term(f, df, i as usize);
            }
        }
        let nz = scores.iter().filter(|&&s| s > 0.0).count();
        if nz == 0 {
            return Vec::new();
        }
        top_indices(&scores, n_top.min(nz))
    }
}

/// The `n` best indices, sorted by decreasing score then increasing index
/// (a total order, so results are reproducible on score ties).
pub fn top_indices(scores: &[f32], n: usize) -> Vec<usize> {
    let n = n.min(scores.len());
    if n == 0 {
        return Vec::new();
    }
    let mut idx: Vec<usize> = (0..scores.len()).collect();
    if n < scores.len() {
        idx.select_nth_unstable_by(n - 1, |&a, &b| {
            scores[b].total_cmp(&scores[a]).then(a.cmp(&b))
        });
        idx.truncate(n);
    }
    idx.sort_unstable_by(|&a, &b| scores[b].total_cmp(&scores[a]).then(a.cmp(&b)));
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_keeps_compound_and_parts() {
        assert_eq!(
            tokenize("app.example.com"),
            ["app.example.com", "app", "example", "com"]
        );
    }

    #[test]
    fn routes_identifier_queries() {
        assert!(is_identifier_query("port de l'appli budget"));
        assert!(is_identifier_query("my-app.example.com"));
        assert!(!is_identifier_query("comment sauvegarder les photos"));
    }
}
