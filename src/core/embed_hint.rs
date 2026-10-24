//! When the built-in hash embedder is sitting on a large corpus, tell the
//! user how to plug a real embedding server (and, past a second threshold,
//! an external vector store).

use serde_json::{json, Value};

/// Past this many indexed files, local-hash is a weak rerank.
pub const NEURAL_MIN_FILES: usize = 150;
/// Past this many chunks, same: BM25 still finds keywords, not meaning.
pub const NEURAL_MIN_CHUNKS: usize = 1_500;
/// Past this, the in-memory exact scan is the bottleneck — Qdrant (or a peer).
pub const STORE_MIN_CHUNKS: usize = 20_000;

const OLLAMA_TOML: &str = "\
[embed]
backend = \"ollama\"
model = \"qwen3-embedding:0.6b\"";

const QDRANT_TOML: &str = "\
[store]
backend = \"qdrant\"
url = \"http://localhost:6333\"";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upgrade {
    pub files: usize,
    pub chunks: usize,
    pub neural: bool,
    pub store: bool,
}

impl Upgrade {
    pub fn assess(files: usize, chunks: usize, local_hash: bool) -> Option<Self> {
        if !local_hash {
            return None;
        }
        let neural = files >= NEURAL_MIN_FILES || chunks >= NEURAL_MIN_CHUNKS;
        let store = chunks >= STORE_MIN_CHUNKS;
        if !neural && !store {
            return None;
        }
        Some(Self {
            files,
            chunks,
            neural,
            store,
        })
    }

    pub fn assess_embedder_id(files: usize, chunks: usize, embedder_id: &str) -> Option<Self> {
        let local = embedder_id.is_empty() || embedder_id.starts_with("local-hash");
        Self::assess(files, chunks, local)
    }

    /// One line under search results (recipe lives in `doctor` / `status`).
    pub fn render_line(&self) -> String {
        let size = format_size(self.files, self.chunks);
        if self.neural {
            format!(
                "hint  {size} on local-hash — BM25 finds keywords, not meaning. Plug Ollama: `ctxq setup --embed ollama` (see `ctxq doctor`)."
            )
        } else {
            format!("hint  {size} — consider Qdrant. See `ctxq doctor`.")
        }
    }

    /// Multi-line block after `index` / `sync` / `doctor`.
    pub fn render_block(&self) -> String {
        let size = format_size(self.files, self.chunks);
        let mut lines: Vec<String> = Vec::new();
        if self.neural {
            lines.push(format!(
                "hint  {size} indexed with local-hash. Keyword search works; meaning does not."
            ));
            lines.push(
                "      `ctxq setup --embed ollama` writes this and re-embeds. Or save it as ctxq.toml, then `ctxq sync`:"
                    .into(),
            );
            lines.push(String::new());
            lines.push(indented(OLLAMA_TOML));
            lines.push(String::new());
            lines.push(
                "      OpenAI-compatible APIs: backend = \"openai\", model = \"text-embedding-3-small\"."
                    .into(),
            );
        }
        if self.store {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(format!(
                "hint  {size} is enough for an external vector store. After embedding, `ctxq store push`:"
            ));
            lines.push(String::new());
            lines.push(indented(QDRANT_TOML));
        }
        lines.join("\n")
    }

    pub fn to_json(&self) -> Value {
        json!({
            "files": self.files,
            "chunks": self.chunks,
            "neural": self.neural,
            "store": self.store,
            "message": self.render_block(),
            "toml": if self.neural { OLLAMA_TOML } else { QDRANT_TOML },
        })
    }
}

fn format_size(files: usize, chunks: usize) -> String {
    format!("{files} files · {chunks} chunks")
}

fn indented(toml: &str) -> String {
    toml.lines()
        .map(|l| format!("        {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_local_hash_stays_quiet() {
        assert!(Upgrade::assess(20, 80, true).is_none());
    }

    #[test]
    fn neural_backend_never_nags() {
        assert!(Upgrade::assess(10_000, 50_000, false).is_none());
    }

    #[test]
    fn many_files_suggests_ollama() {
        let u = Upgrade::assess(200, 400, true).unwrap();
        assert!(u.neural);
        assert!(!u.store);
        let block = u.render_block();
        assert!(block.contains("backend = \"ollama\""));
        assert!(block.contains("qwen3-embedding:0.6b"));
        assert!(block.contains("ctxq sync"));
        assert!(!block.contains("qdrant"));
    }

    #[test]
    fn many_chunks_suggests_qdrant_too() {
        let u = Upgrade::assess(80, 25_000, true).unwrap();
        assert!(u.neural);
        assert!(u.store);
        let block = u.render_block();
        assert!(block.contains("ollama"));
        assert!(block.contains("qdrant"));
    }

    #[test]
    fn embedder_id_detects_local_hash() {
        assert!(Upgrade::assess_embedder_id(200, 10, "local-hash-v1-128").is_some());
        assert!(Upgrade::assess_embedder_id(200, 10, "http:qwen3:abc").is_none());
    }
}
