//! Embeddings served over HTTP.
//!
//! Two protocols cover most of what exists:
//!   - **Ollama** (`POST /api/embed`, batched);
//!   - **OpenAI-compatible** (`POST /v1/embeddings`), which also covers
//!     LM Studio, llama.cpp server, vLLM, text-embeddings-inference, Infinity.
//!
//! Several instances can be declared: they are tried in order and the one
//! that answers becomes the preferred one (fast machine first, local
//! fallback behind). The model's task templates are applied here: omitting
//! them degrades relevance without raising any error.

use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Ollama,
    OpenAi,
}

#[derive(Debug, Clone)]
pub struct HttpSettings {
    pub backend: Backend,
    pub urls: Vec<String>,
    pub model: String,
    pub api_key: Option<String>,
    /// Truncation in characters before sending: a deterministic index whatever
    /// the server (depending on the implementation, an overlong text is
    /// silently truncated or rejected).
    pub max_chars: usize,
    pub doc_template: String,
    pub query_template: String,
    /// Maximum simultaneous requests to the service. 1 by default: a shared
    /// embedding server (used by other tools too) must not be starved by a
    /// rebuild.
    pub concurrency: usize,
    /// Maximum texts per call: bounds the wait of an interactive request
    /// queued behind a batch on the server.
    pub batch: usize,
    pub connect_timeout_secs: u64,
    pub read_timeout_secs: u64,
}

impl HttpSettings {
    pub fn from_settings(s: &crate::core::settings::Settings, backend: Backend) -> Self {
        let model = s.embed_model();
        let (doc_template, query_template) = s.embed.templates_for(&model);
        Self {
            backend,
            urls: s.embed_urls(),
            model,
            api_key: s.embed.api_key(),
            max_chars: s.embed.max_chars,
            doc_template,
            query_template,
            concurrency: s.embed.concurrency.max(1),
            batch: s.embed.batch.max(1),
            connect_timeout_secs: s.embed.connect_timeout_secs,
            read_timeout_secs: s.embed.read_timeout_secs,
        }
    }
}

#[derive(Deserialize)]
struct OllamaResponse {
    embeddings: Vec<Vec<f32>>,
}

#[derive(Deserialize)]
struct OpenAiResponse {
    data: Vec<OpenAiDatum>,
}

#[derive(Deserialize)]
struct OpenAiDatum {
    embedding: Vec<f32>,
}

enum EmbedError {
    ContextTooLong,
    Other(anyhow::Error),
}

pub struct HttpEmbedder {
    agent: ureq::Agent,
    settings: HttpSettings,
    preferred: Mutex<usize>,
    /// Known once the first vector arrives; probed only if asked for earlier.
    dim: std::sync::OnceLock<usize>,
    id: String,
    in_flight: (Mutex<usize>, std::sync::Condvar),
    /// Query → vector. Remote query embedding dominates a warm search (~200 ms
    /// on a small CPU box vs ~15 ms for the search): agents repeat questions.
    query_cache: Mutex<QueryCache>,
}

/// Small LRU keyed by the query text.
#[derive(Default)]
struct QueryCache {
    map: std::collections::HashMap<String, (u64, Vec<f32>)>,
    tick: u64,
}

const QUERY_CACHE_SIZE: usize = 1024;

impl QueryCache {
    fn get(&mut self, k: &str) -> Option<Vec<f32>> {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(k).map(|e| {
            e.0 = tick;
            e.1.clone()
        })
    }

    fn put(&mut self, k: String, v: Vec<f32>) {
        if self.map.len() >= QUERY_CACHE_SIZE {
            if let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.0)
                .map(|(k, _)| k.clone())
            {
                self.map.remove(&oldest);
            }
        }
        self.tick += 1;
        self.map.insert(k, (self.tick, v));
    }
}

impl std::fmt::Debug for HttpEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpEmbedder")
            .field("id", &self.id)
            .finish()
    }
}

impl HttpEmbedder {
    /// Builds the embedder, without any network call.
    pub fn new(settings: HttpSettings) -> Result<Self> {
        if settings.urls.is_empty() {
            bail!("no embedding instance configured (CTXQ_EMBED_URLS)");
        }
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(settings.connect_timeout_secs))
            .timeout_read(Duration::from_secs(settings.read_timeout_secs))
            .build();
        Ok(Self {
            agent,
            preferred: Mutex::new(0),
            dim: std::sync::OnceLock::new(),
            // The model AND the templates make up the index identity: changing
            // either without rebuilding would return wrong results.
            id: format!(
                "http:{}:{:016x}",
                settings.model,
                crate::core::vector::hash_str(&format!(
                    "{}\u{0}{}",
                    settings.doc_template, settings.query_template
                ))
            ),
            settings,
            in_flight: (Mutex::new(0), std::sync::Condvar::new()),
            query_cache: Mutex::new(QueryCache::default()),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Vector dimension; probes the service if none has been received yet
    /// (0 if unreachable).
    pub fn dim(&self) -> usize {
        if let Some(d) = self.dim.get() {
            return *d;
        }
        match self.embed_raw(&["dimension".to_string()]) {
            Ok(_) => self.dim.get().copied().unwrap_or(0),
            Err(e) => {
                tracing::warn!("embedding service probe: {e:#}");
                0
            }
        }
    }

    pub fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let payloads: Vec<String> = texts
            .iter()
            .map(|t| self.settings.doc_template.replace("{t}", t))
            .collect();
        self.embed_raw(&payloads)
    }

    pub fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        if let Some(v) = self.query_cache.lock().unwrap().get(query) {
            return Ok(v);
        }
        let q = self.settings.query_template.replace("{q}", query);
        let v = self
            .embed_raw(&[q])?
            .pop()
            .ok_or_else(|| anyhow!("response without embedding"))?;
        self.query_cache
            .lock()
            .unwrap()
            .put(query.to_string(), v.clone());
        Ok(v)
    }

    /// Batch truncated then normalised (the dot product then equals the cosine).
    /// If the service complains that the context is exceeded, retry text by
    /// text, halving the one that does not fit.
    fn embed_raw(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let bodies: Vec<String> = texts
            .iter()
            .map(|t| t.chars().take(self.settings.max_chars).collect())
            .collect();
        let mut out = Vec::with_capacity(bodies.len());
        for lot in bodies.chunks(self.settings.batch) {
            match self.try_all(lot) {
                Ok(v) => out.extend(v),
                Err(EmbedError::ContextTooLong) => {
                    for b in lot {
                        out.push(self.embed_one_shrinking(b.clone())?);
                    }
                }
                Err(EmbedError::Other(e)) => return Err(e),
            }
        }
        if out.len() != texts.len() {
            bail!("{} vectors received for {} texts", out.len(), texts.len());
        }
        if let Some(first) = out.first() {
            let _ = self.dim.set(first.len());
        }
        for v in &mut out {
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if n > 0.0 {
                v.iter_mut().for_each(|x| *x /= n);
            }
        }
        Ok(out)
    }

    fn embed_one_shrinking(&self, mut body: String) -> Result<Vec<f32>> {
        loop {
            match self.try_all(std::slice::from_ref(&body)) {
                Ok(mut v) => return v.pop().ok_or_else(|| anyhow!("response without embedding")),
                Err(EmbedError::ContextTooLong) if body.chars().count() > 200 => {
                    let keep = (body.chars().count() / 2).max(200);
                    body = body.chars().take(keep).collect();
                }
                Err(EmbedError::ContextTooLong) => {
                    bail!("irreducible text rejected by the embedding service")
                }
                Err(EmbedError::Other(e)) => return Err(e),
            }
        }
    }

    fn try_all(&self, texts: &[String]) -> std::result::Result<Vec<Vec<f32>>, EmbedError> {
        let urls = &self.settings.urls;
        let start = *self.preferred.lock().unwrap();
        let mut last = None;
        for attempt in 0..urls.len() {
            let i = (start + attempt) % urls.len();
            match self.call(&urls[i], texts) {
                Ok(v) => {
                    *self.preferred.lock().unwrap() = i;
                    return Ok(v);
                }
                Err(EmbedError::ContextTooLong) => return Err(EmbedError::ContextTooLong),
                // Instance unreachable: move on to the next one.
                Err(EmbedError::Other(e)) => last = Some(e),
            }
        }
        Err(EmbedError::Other(last.unwrap_or_else(|| {
            anyhow!("no embedding instance reachable")
        })))
    }

    fn call(&self, base: &str, texts: &[String]) -> std::result::Result<Vec<Vec<f32>>, EmbedError> {
        let _permit = self.acquire();
        let model = &self.settings.model;
        let (url, payload) = match self.settings.backend {
            Backend::Ollama => (
                format!("{base}/api/embed"),
                serde_json::json!({ "model": model, "input": texts, "keep_alive": "24h", "truncate": false }),
            ),
            Backend::OpenAi => (
                format!("{base}/v1/embeddings"),
                serde_json::json!({ "model": model, "input": texts }),
            ),
        };
        let mut req = self.agent.post(&url);
        if let Some(key) = &self.settings.api_key {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
        let bad =
            |e: std::io::Error| EmbedError::Other(anyhow!("unreadable response from {url}: {e}"));
        match req.send_json(payload) {
            Ok(resp) => match self.settings.backend {
                Backend::Ollama => resp
                    .into_json::<OllamaResponse>()
                    .map(|r| r.embeddings)
                    .map_err(bad),
                Backend::OpenAi => resp
                    .into_json::<OpenAiResponse>()
                    .map(|r| r.data.into_iter().map(|d| d.embedding).collect())
                    .map_err(bad),
            },
            Err(ureq::Error::Status(code, resp)) => {
                let msg = resp.into_string().unwrap_or_default();
                if [
                    "too large",
                    "exceeds",
                    "context length",
                    "maximum context",
                    "input length",
                ]
                .iter()
                .any(|k| msg.contains(k))
                {
                    return Err(EmbedError::ContextTooLong);
                }
                Err(EmbedError::Other(anyhow!("{url} answered {code}: {msg}")))
            }
            Err(e) => Err(EmbedError::Other(
                anyhow::Error::new(e).context(format!("call to {url}")),
            )),
        }
    }
}

/// Concurrency token, released on drop.
struct Permit<'a>(&'a (Mutex<usize>, std::sync::Condvar));

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        *self.0 .0.lock().unwrap() -= 1;
        self.0 .1.notify_one();
    }
}

impl HttpEmbedder {
    fn acquire(&self) -> Permit<'_> {
        let (lock, cv) = &self.in_flight;
        let mut n = cv
            .wait_while(lock.lock().unwrap(), |n| *n >= self.settings.concurrency)
            .unwrap();
        *n += 1;
        Permit(&self.in_flight)
    }
}

#[cfg(test)]
mod tests {
    use crate::core::settings::Settings;

    #[test]
    fn picks_family_templates() {
        let s = Settings::default();
        assert_eq!(
            s.embed.templates_for("nomic-embed-text").1,
            "search_query: {q}"
        );
        assert!(s
            .embed
            .templates_for("qwen3-embedding:0.6b")
            .1
            .starts_with("Instruct:"));
        assert_eq!(
            s.embed.templates_for("bge-m3"),
            ("{t}".into(), "{q}".into())
        );
    }
}
