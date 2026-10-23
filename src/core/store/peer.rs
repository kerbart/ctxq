//! Another documentum instance, queried through its daemon's HTTP API
//! (`POST /search`). Read-only by nature: the peer embeds the query with its
//! own model, so no vector crosses the wire and models need not match.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use super::{HitPayload, StoreHit, StorePoint, VectorStore};
use crate::core::settings::StoreSettings;

pub struct PeerStore {
    agent: ureq::Agent,
    base: String,
    api_key: Option<String>,
}

impl PeerStore {
    pub fn new(s: &StoreSettings) -> Result<Self> {
        if s.url.is_empty() {
            bail!("[[store.federate]] peer: url is required (e.g. http://other-host:7700)");
        }
        Ok(Self {
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(3))
                .timeout_read(Duration::from_secs(s.timeout_secs.max(1)))
                .build(),
            base: s.url.trim_end_matches('/').to_string(),
            api_key: s.api_key(),
        })
    }

    /// Text search on the peer (it embeds the query itself).
    pub fn search_text(&self, query: &str, k: usize) -> Result<Vec<StoreHit>> {
        let url = format!("{}/search", self.base);
        let mut req = self.agent.post(&url);
        if let Some(key) = &self.api_key {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
        let resp: Value = match req.send_json(json!({ "query": query, "limit": k })) {
            Ok(r) => r.into_json().with_context(|| format!("reading {url}"))?,
            Err(ureq::Error::Status(code, r)) => {
                return Err(anyhow!(
                    "{url} → {code}: {}",
                    r.into_string().unwrap_or_default()
                ))
            }
            Err(e) => return Err(anyhow::Error::new(e).context(url)),
        };
        Ok(resp["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|h| {
                let s = |f: &str| h[f].as_str().unwrap_or("").to_string();
                let lines = h["lines"].as_array().cloned().unwrap_or_default();
                StoreHit {
                    chunk_id: format!(
                        "{}#{}",
                        s("source"),
                        lines.first().and_then(Value::as_u64).unwrap_or(0)
                    ),
                    score: h["score"].as_f64().unwrap_or(0.0) as f32,
                    point: Some(HitPayload {
                        source: s("source"),
                        title: s("breadcrumb"),
                        text: s("text"),
                        start_line: lines.first().and_then(Value::as_u64).map(|v| v as usize),
                        end_line: lines.get(1).and_then(Value::as_u64).map(|v| v as usize),
                    }),
                }
            })
            .collect())
    }
}

impl VectorStore for PeerStore {
    fn label(&self) -> String {
        format!("peer:{}", self.base)
    }
    fn ensure(&self, _dim: usize) -> Result<()> {
        bail!("a peer is read-only")
    }
    fn upsert(&self, _points: &[StorePoint]) -> Result<()> {
        bail!("a peer is read-only")
    }
    fn delete_sources(&self, _sources: &[String]) -> Result<()> {
        bail!("a peer is read-only")
    }
    fn search(&self, _query: &[f32], _k: usize, _only: Option<&[String]>) -> Result<Vec<StoreHit>> {
        bail!("a peer is searched by text (search_text), not by vector")
    }
    fn count(&self) -> Result<usize> {
        Ok(0)
    }
    fn query(&self, text: &str, _vector: &[f32], k: usize) -> Result<Vec<StoreHit>> {
        self.search_text(text, k)
    }
}
