//! Embeddings: local 128d hashing, HTTP (Ollama / OpenAI), optional ONNX.

use std::sync::Arc;

use anyhow::{bail, Result};
use regex::Regex;
use std::sync::LazyLock;

use crate::core::embed_http::{Backend, HttpEmbedder, HttpSettings};
use crate::core::settings::{EmbedBackend, Settings};

/// 64-bit FNV-1a. Stable from one process to the next (unlike `HashMap`'s SipHash).
pub fn hash_str(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

const LOCAL_DIM: usize = 128;
const LOCAL_ID: &str = "local-hash-v1-128";

static WORD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9_.]+").expect("word"));

fn sign_of(key: &str) -> f32 {
    if hash_str(&format!("{key}_sign")) & 1 == 0 {
        1.0
    } else {
        -1.0
    }
}

fn add_feat(vec: &mut [f32], key: &str, weight: f32) {
    let idx = (hash_str(key) % LOCAL_DIM as u64) as usize;
    vec[idx] += sign_of(key) * weight;
}

/// Dense 128d vector, L2-normalised. Identifier: [`LOCAL_ID`].
pub fn local_embed(text: &str) -> Vec<f32> {
    let mut vec = vec![0f32; LOCAL_DIM];
    let words: Vec<String> = WORD_RE
        .find_iter(text)
        .map(|m| m.as_str().to_lowercase())
        .filter(|w| !w.is_empty())
        .collect();
    for (i, word) in words.iter().enumerate() {
        add_feat(&mut vec, word, 1.5);
        let chars: Vec<char> = word.chars().collect();
        if chars.len() >= 3 {
            for tri in chars.windows(3) {
                let gram: String = tri.iter().collect();
                add_feat(&mut vec, &gram, 0.5);
            }
        }
        if i + 1 < words.len() {
            add_feat(&mut vec, &format!("{} {}", word, words[i + 1]), 1.0);
        }
    }
    let norm = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-6 {
        for x in &mut vec {
            *x /= norm;
        }
    } else {
        vec.fill(0.0);
    }
    vec
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom <= 1e-6 {
        0.0
    } else {
        dot / denom
    }
}

#[cfg(feature = "onnx-embeddings")]
use std::path::PathBuf;

#[cfg(feature = "onnx-embeddings")]
struct OnnxEmbedder {
    model: std::sync::Mutex<Option<fastembed::TextEmbedding>>,
    failed: std::sync::atomic::AtomicBool,
}

#[cfg(feature = "onnx-embeddings")]
impl OnnxEmbedder {
    fn new() -> Self {
        Self {
            model: std::sync::Mutex::new(None),
            failed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn cache_dir() -> PathBuf {
        if let Some(p) = std::env::var_os("CTXQ_ONNX_CACHE") {
            return PathBuf::from(p);
        }
        if let Some(p) = std::env::var_os("FASTEMBED_CACHE_PATH") {
            return PathBuf::from(p);
        }
        if let Some(p) = std::env::var_os("CTXQ_HOME_DIR") {
            return PathBuf::from(p).join(".ctxq/models");
        }
        if let Some(p) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return PathBuf::from(p).join(".ctxq/models");
        }
        std::env::temp_dir().join("ctxq-models")
    }

    fn load(&self) -> bool {
        if self.failed.load(std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        let mut slot = self.model.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return true;
        }
        let cache = Self::cache_dir();
        let _ = std::fs::create_dir_all(&cache);
        match fastembed::TextEmbedding::try_new(
            fastembed::InitOptions::new(fastembed::EmbeddingModel::BGESmallENV15)
                .with_cache_dir(cache)
                .with_show_download_progress(false),
        ) {
            Ok(m) => {
                *slot = Some(m);
                true
            }
            Err(e) => {
                tracing::warn!("ONNX bge-small indisponible ({e}); vecteur nul");
                self.failed
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                false
            }
        }
    }

    fn embed(&self, texts: &[String]) -> Vec<Vec<f32>> {
        let zeros = |n: usize| vec![vec![0f32; 384]; n];
        if !self.load() {
            return zeros(texts.len());
        }
        let mut slot = self.model.lock().unwrap_or_else(|e| e.into_inner());
        let Some(model) = slot.as_mut() else {
            return zeros(texts.len());
        };
        match model.embed(texts.to_vec(), None) {
            Ok(v) if v.len() == texts.len() => v,
            Ok(_) => {
                tracing::warn!("ONNX returned an unexpected number of vectors");
                zeros(texts.len())
            }
            Err(e) => {
                tracing::warn!("ONNX inference failed ({e}); using a zero vector");
                zeros(texts.len())
            }
        }
    }
}

enum Kind {
    Local,
    Http(Box<HttpEmbedder>),
    #[cfg(feature = "onnx-embeddings")]
    Onnx(Box<OnnxEmbedder>),
}

#[derive(Clone)]
pub struct Embedder {
    kind: Arc<Kind>,
}

impl std::fmt::Debug for Embedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Embedder").field("id", &self.id()).finish()
    }
}

impl Embedder {
    pub fn local() -> Self {
        Self {
            kind: Arc::new(Kind::Local),
        }
    }

    pub fn from_settings(settings: &Settings, backend: Option<EmbedBackend>) -> Result<Self> {
        let backend = backend.unwrap_or(settings.embed.backend);
        match backend {
            EmbedBackend::Local => Ok(Self::local()),
            EmbedBackend::Ollama => Ok(Self {
                kind: Arc::new(Kind::Http(Box::new(HttpEmbedder::new(
                    HttpSettings::from_settings(settings, Backend::Ollama),
                )?))),
            }),
            EmbedBackend::OpenAi => Ok(Self {
                kind: Arc::new(Kind::Http(Box::new(HttpEmbedder::new(
                    HttpSettings::from_settings(settings, Backend::OpenAi),
                )?))),
            }),
            EmbedBackend::Onnx => {
                #[cfg(feature = "onnx-embeddings")]
                {
                    Ok(Self {
                        kind: Arc::new(Kind::Onnx(Box::new(OnnxEmbedder::new()))),
                    })
                }
                #[cfg(not(feature = "onnx-embeddings"))]
                {
                    bail!("onnx backend requested: rebuild with --features onnx-embeddings")
                }
            }
        }
    }

    pub fn id(&self) -> String {
        match self.kind.as_ref() {
            Kind::Local => LOCAL_ID.to_string(),
            Kind::Http(h) => h.id().to_string(),
            #[cfg(feature = "onnx-embeddings")]
            Kind::Onnx(_) => "bge-small-en-v1.5-384".to_string(),
        }
    }

    pub fn dim(&self) -> usize {
        match self.kind.as_ref() {
            Kind::Local => LOCAL_DIM,
            Kind::Http(h) => h.dim(),
            #[cfg(feature = "onnx-embeddings")]
            Kind::Onnx(_) => 384,
        }
    }

    /// Hashed n-grams, not a neural model. Fusion must let BM25 lead.
    pub fn is_local_hash(&self) -> bool {
        matches!(self.kind.as_ref(), Kind::Local)
    }

    pub fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        match self.kind.as_ref() {
            Kind::Local => Ok(texts.iter().map(|t| local_embed(t)).collect()),
            Kind::Http(h) => h.embed_documents(texts),
            #[cfg(feature = "onnx-embeddings")]
            Kind::Onnx(o) => Ok(o.embed(texts)),
        }
    }

    pub fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        match self.kind.as_ref() {
            Kind::Local => Ok(local_embed(text)),
            Kind::Http(h) => h.embed_query(text),
            #[cfg(feature = "onnx-embeddings")]
            Kind::Onnx(o) => Ok(o
                .embed(&[text.to_string()])
                .pop()
                .unwrap_or_else(|| vec![0.0; 384])),
        }
    }

    pub fn cosine_similarity(&self, a: &[f32], b: &[f32]) -> f32 {
        cosine_similarity(a, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_embedder_is_128d_normalized_and_stable() {
        let e = Embedder::local();
        assert_eq!(e.id(), "local-hash-v1-128");
        assert_eq!(e.dim(), 128);
        let a = e.embed_documents(&["port 6239 alpha".into()]).unwrap();
        let b = local_embed("port 6239 alpha");
        assert_eq!(a[0], b);
        assert_eq!(a[0].len(), 128);
        let norm = a[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "{norm}");
        assert_ne!(local_embed("autre texte"), b);
        assert_eq!(local_embed(""), vec![0f32; 128]);
        assert!((e.cosine_similarity(&a[0], &b) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn hash_str_is_stable() {
        assert_eq!(hash_str("abc"), hash_str("abc"));
        assert_ne!(hash_str("abc"), hash_str("abd"));
    }
}
