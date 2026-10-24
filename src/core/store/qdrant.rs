//! Qdrant over its REST API (no client library: a handful of JSON calls).

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use super::{HitPayload, StoreHit, StorePoint, VectorStore};
use crate::core::settings::{StoreMode, StoreSettings};

pub struct QdrantStore {
    agent: ureq::Agent,
    base: String,
    collection: String,
    api_key: Option<String>,
    mode: StoreMode,
    exact: bool,
    text_field: String,
    source_field: String,
    title_field: String,
}

/// Qdrant point ids are integers or UUIDs: chunk ids (16 hex digits) map to a
/// stable UUID, and the chunk id itself rides in the payload.
pub fn point_uuid(chunk_id: &str) -> String {
    let hex: String = if chunk_id.len() == 16 && chunk_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        format!("{chunk_id:0>32}")
    } else {
        use sha2::{Digest, Sha256};
        Sha256::digest(chunk_id.as_bytes())
            .iter()
            .take(16)
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

impl QdrantStore {
    pub fn new(s: &StoreSettings) -> Result<Self> {
        if s.url.is_empty() {
            bail!("[store] url is required for qdrant (e.g. http://localhost:6333)");
        }
        Ok(Self {
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(3))
                .timeout_read(Duration::from_secs(s.timeout_secs.max(1)))
                .build(),
            base: s.url.trim_end_matches('/').to_string(),
            collection: s.collection_name(),
            api_key: s.api_key(),
            mode: s.mode,
            exact: s.exact,
            text_field: s.text_field.clone(),
            source_field: s.source_field.clone(),
            title_field: s.title_field.clone(),
        })
    }

    fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("{}/collections/{}{path}", self.base, self.collection);
        let mut req = self.agent.request(method, &url);
        if let Some(k) = &self.api_key {
            req = req.set("api-key", k);
        }
        let resp = match body {
            Some(b) => req.send_json(b),
            None => req.call(),
        };
        match resp {
            Ok(r) => r
                .into_json::<Value>()
                .with_context(|| format!("reading {url}")),
            Err(ureq::Error::Status(code, r)) => {
                let msg = r.into_string().unwrap_or_default();
                Err(anyhow!("{method} {url} → {code}: {msg}"))
            }
            Err(e) => Err(anyhow::Error::new(e).context(format!("{method} {url}"))),
        }
    }

    fn owned(&self) -> Result<()> {
        if self.mode == StoreMode::Attached {
            bail!(
                "{} is attached (read-only): ctxq never writes to it",
                self.label()
            );
        }
        Ok(())
    }
}

impl VectorStore for QdrantStore {
    fn label(&self) -> String {
        format!("qdrant:{}/{}", self.base, self.collection)
    }

    fn ensure(&self, dim: usize) -> Result<()> {
        self.owned()?;
        match self.call("GET", "", None) {
            Ok(info) => {
                let size = info["result"]["config"]["params"]["vectors"]["size"].as_u64();
                if size.is_some_and(|s| s as usize != dim) {
                    bail!(
                        "{} holds {}-d vectors, the active embedder produces {dim}-d: \
                         use another collection or recreate it",
                        self.label(),
                        size.unwrap_or(0)
                    );
                }
                Ok(())
            }
            Err(_) => {
                self.call(
                    "PUT",
                    "",
                    Some(json!({ "vectors": { "size": dim, "distance": "Cosine" } })),
                )?;
                // Payload index: deleting a file's points filters on its path.
                self.call(
                    "PUT",
                    "/index",
                    Some(json!({ "field_name": "source", "field_schema": "keyword" })),
                )?;
                Ok(())
            }
        }
    }

    fn upsert(&self, points: &[StorePoint]) -> Result<()> {
        self.owned()?;
        for batch in points.chunks(256) {
            let pts: Vec<Value> = batch
                .iter()
                .map(|p| {
                    json!({
                        "id": point_uuid(&p.chunk_id),
                        "vector": p.vector,
                        "payload": {
                            "chunk_id": p.chunk_id,
                            "source": p.source,
                            "title": p.breadcrumb,
                            "text": p.text,
                            "start_line": p.start_line,
                            "end_line": p.end_line,
                        }
                    })
                })
                .collect();
            self.call("PUT", "/points?wait=true", Some(json!({ "points": pts })))?;
        }
        Ok(())
    }

    fn delete_sources(&self, sources: &[String]) -> Result<()> {
        self.owned()?;
        for batch in sources.chunks(256) {
            self.call(
                "POST",
                "/points/delete?wait=true",
                Some(json!({ "filter": { "must": [ { "key": "source", "match": { "any": batch } } ] } })),
            )?;
        }
        Ok(())
    }

    fn search(&self, query: &[f32], k: usize, only: Option<&[String]>) -> Result<Vec<StoreHit>> {
        if query.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        let mut body = json!({
            "vector": query,
            "limit": k,
            "with_payload": true,
            "params": { "exact": self.exact, "hnsw_ef": (k * 2).max(128) },
        });
        if let Some(ids) = only {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let uuids: Vec<String> = ids.iter().map(|i| point_uuid(i)).collect();
            body["filter"] = json!({ "must": [ { "has_id": uuids } ] });
        }
        let resp = self.call("POST", "/points/search", Some(body))?;
        let hits = resp["result"].as_array().cloned().unwrap_or_default();
        Ok(hits
            .into_iter()
            .map(|h| {
                let pl = &h["payload"];
                let s = |f: &str| pl[f].as_str().unwrap_or("").to_string();
                let chunk_id = pl["chunk_id"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| h["id"].to_string().trim_matches('"').to_string());
                StoreHit {
                    chunk_id,
                    score: h["score"].as_f64().unwrap_or(0.0) as f32,
                    point: (self.mode == StoreMode::Attached).then(|| HitPayload {
                        source: s(&self.source_field),
                        title: s(&self.title_field),
                        text: s(&self.text_field),
                        start_line: pl["start_line"].as_u64().map(|v| v as usize),
                        end_line: pl["end_line"].as_u64().map(|v| v as usize),
                    }),
                }
            })
            .collect())
    }

    fn count(&self) -> Result<usize> {
        let r = self.call("POST", "/points/count", Some(json!({ "exact": true })))?;
        Ok(r["result"]["count"].as_u64().unwrap_or(0) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_ids_map_to_stable_uuids() {
        assert_eq!(
            point_uuid("00ff00ff00ff00ff"),
            "00000000-0000-0000-00ff-00ff00ff00ff"
        );
        let u = point_uuid("not-hex-id");
        assert_eq!(u.len(), 36);
        assert_eq!(u, point_uuid("not-hex-id"));
    }
}
