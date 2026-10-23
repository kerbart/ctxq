//! Vector stores. The index is local by default (vectors in its SQLite file,
//! scanned exactly in memory); `[store]` can move the dense channel to an
//! external base, and `[[store.federate]]` adds read-only bases queried with
//! every search.
//!
//! Lexical search, chunks, metadata and the embedding cache always stay local:
//! BM25 is what finds exact identifiers, and it must see the whole corpus.

pub mod peer;
pub mod qdrant;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::core::settings::{StoreBackend, StoreMode, StoreSettings};

/// A chunk vector with what an external store needs to answer on its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorePoint {
    pub chunk_id: String,
    pub vector: Vec<f32>,
    pub source: String,
    pub breadcrumb: String,
    pub text: String,
    pub start_line: usize,
    pub end_line: usize,
}

/// One dense hit. `point` is filled by stores that carry their own payload
/// (attached / federated); owned stores return ids only.
#[derive(Debug, Clone)]
pub struct StoreHit {
    pub chunk_id: String,
    pub score: f32,
    pub point: Option<HitPayload>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HitPayload {
    pub source: String,
    pub title: String,
    pub text: String,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
}

pub trait VectorStore: Send + Sync {
    /// Human label (`qdrant:http://…/documentum`).
    fn label(&self) -> String;

    /// Creates the collection if needed (owned stores).
    fn ensure(&self, dim: usize) -> Result<()>;

    fn upsert(&self, points: &[StorePoint]) -> Result<()>;

    /// Removes every point of these source files.
    fn delete_sources(&self, sources: &[String]) -> Result<()>;

    /// Top `k` by cosine. `only` restricts scoring to these chunk ids (used to
    /// score lexical candidates that the dense pool missed).
    fn search(&self, query: &[f32], k: usize, only: Option<&[String]>) -> Result<Vec<StoreHit>>;

    fn count(&self) -> Result<usize>;

    /// Federated query: by vector for vector stores; a peer re-embeds `text`
    /// with its own model instead.
    fn query(&self, _text: &str, vector: &[f32], k: usize) -> Result<Vec<StoreHit>> {
        self.search(vector, k, None)
    }
}

/// The external store of `[store]`, if not local.
pub fn open_primary(s: &StoreSettings) -> Result<Option<Box<dyn VectorStore>>> {
    match s.backend {
        StoreBackend::Local => Ok(None),
        StoreBackend::Qdrant => Ok(Some(Box::new(qdrant::QdrantStore::new(s)?))),
        StoreBackend::Peer => {
            bail!("[store] backend = \"peer\" is for federation only ([[store.federate]])")
        }
    }
}

/// Read-only stores of `[[store.federate]]`.
pub fn open_federated(s: &StoreSettings) -> Result<Vec<(StoreSettings, Box<dyn VectorStore>)>> {
    let mut out: Vec<(StoreSettings, Box<dyn VectorStore>)> = Vec::new();
    for f in &s.federate {
        let mut f = f.clone();
        f.mode = StoreMode::Attached;
        let store: Box<dyn VectorStore> = match f.backend {
            StoreBackend::Qdrant => Box::new(qdrant::QdrantStore::new(&f)?),
            StoreBackend::Peer => Box::new(peer::PeerStore::new(&f)?),
            StoreBackend::Local => bail!("[[store.federate]]: backend must be qdrant or peer"),
        };
        out.push((f, store));
    }
    Ok(out)
}
