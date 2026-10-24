//! Embedding provider for `ctxq setup`. One numbered question, or `--embed`.
//! Writing `[embed]` is pure: the caller picks the file and does the I/O.

use std::path::Path;

use toml_edit::{Array, DocumentMut, Item, Table};

pub const OLLAMA_MODEL: &str = "qwen3-embedding:0.6b";
pub const OPENAI_MODEL: &str = "text-embedding-3-small";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedChoice {
    pub backend: String,
    pub model: Option<String>,
    pub url: Option<String>,
    pub api_key_env: Option<String>,
}

impl EmbedChoice {
    pub fn local() -> Self {
        Self {
            backend: "local".into(),
            model: None,
            url: None,
            api_key_env: None,
        }
    }

    pub fn onnx() -> Self {
        Self {
            backend: "onnx".into(),
            model: None,
            url: None,
            api_key_env: None,
        }
    }

    pub fn ollama(model: Option<String>, url: Option<String>) -> Self {
        Self {
            backend: "ollama".into(),
            model: Some(model.unwrap_or_else(|| OLLAMA_MODEL.into())),
            url,
            api_key_env: None,
        }
    }

    pub fn openai(model: Option<String>, url: Option<String>) -> Self {
        let api_key_env = url.as_ref().and_then(|u| {
            if u.contains("openai.com") {
                Some("OPENAI_API_KEY".into())
            } else {
                None
            }
        });
        Self {
            backend: "openai".into(),
            model: Some(model.unwrap_or_else(|| OPENAI_MODEL.into())),
            url,
            api_key_env,
        }
    }

    pub fn is_default_local(&self) -> bool {
        self.backend == "local" && self.model.is_none() && self.url.is_none() && self.api_key_env.is_none()
    }

    pub fn summary(&self) -> String {
        let mut out = match self.model.as_deref() {
            Some(model) if self.backend == "ollama" || self.backend == "openai" => {
                format!("{} {model}", self.backend)
            }
            _ => self.backend.clone(),
        };
        if let Some(url) = &self.url {
            out.push_str(" @ ");
            out.push_str(url);
        }
        out
    }
}

struct Row {
    number: usize,
    id: &'static str,
    label: &'static str,
    detail: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        number: 1,
        id: "local",
        label: "local",
        detail: "hashed n-grams, offline — BM25 leads",
    },
    Row {
        number: 2,
        id: "ollama",
        label: "ollama",
        detail: "qwen3-embedding:0.6b at http://localhost:11434",
    },
    Row {
        number: 3,
        id: "openai",
        label: "openai",
        detail: "text-embedding-3-small at http://localhost:8080",
    },
    Row {
        number: 4,
        id: "onnx",
        label: "onnx",
        detail: "embedded bge-small (build feature onnx-embeddings)",
    },
];

pub fn render_embed_menu(current_backend: &str, current_label: &str) -> String {
    let mut out = format!(
        "Embedding provider? Empty line keeps the current one ({current_label}).\n\
A different provider rebuilds vectors on this setup.\n\
Custom model: ollama:MODEL or ollama:MODEL@http://host:11434\n\n"
    );
    for row in ROWS {
        let mark = if row.id == current_backend { "*" } else { " " };
        out.push_str(&format!(
            "  {:>2} {mark} {:<16} {}\n",
            row.number, row.label, row.detail
        ));
    }
    out
}

/// Empty keeps the current provider (`None`). A number, a name (`ollama`,
/// `qwen`), or a spec (`ollama:nomic-embed-text@http://host:11434`).
pub fn parse_embed_menu_line(line: &str) -> Result<Option<EmbedChoice>, String> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    if let Some(choice) = preset(line) {
        return Ok(Some(choice));
    }
    Ok(Some(parse_embed_spec(line)?))
}

fn preset(token: &str) -> Option<EmbedChoice> {
    let token = token.trim();
    if token.contains(':') || token.contains('@') {
        return None;
    }
    Some(match token.to_ascii_lowercase().as_str() {
        "1" | "local" => EmbedChoice::local(),
        "2" | "ollama" | "qwen" | "qwen3" => EmbedChoice::ollama(None, None),
        "3" | "openai" => EmbedChoice::openai(None, None),
        "4" | "onnx" => EmbedChoice::onnx(),
        _ => return None,
    })
}

/// `local` | `onnx` | `ollama` | `openai` | `qwen`, optional `:MODEL` and `@URL`.
/// A bare `qwen3-embedding:0.6b` means Ollama with that model.
pub fn parse_embed_spec(spec: &str) -> Result<EmbedChoice, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("empty embedding provider".into());
    }
    if let Some(choice) = preset(spec) {
        return Ok(choice);
    }
    let (left, url) = match spec.split_once('@') {
        Some((left, url)) => {
            let url = url.trim().trim_end_matches('/');
            if url.is_empty() {
                return Err("empty url after @".into());
            }
            (left.trim(), Some(url.to_string()))
        }
        None => (spec, None),
    };
    if left.is_empty() {
        return Err("put the provider before @, for example ollama@http://localhost:11434".into());
    }
    if let Some((backend, model)) = split_backend(left) {
        let model = {
            let model = model.trim();
            if model.is_empty() {
                None
            } else {
                Some(model.to_string())
            }
        };
        return finish(backend, model, url);
    }
    if left.contains("://") {
        return Err("put the url after @, for example ollama@http://localhost:11434".into());
    }
    if left.contains(':') {
        return Ok(EmbedChoice::ollama(Some(left.to_string()), url));
    }
    Err(format!(
        "unknown embedding provider {left:?}. Use local, ollama, qwen, openai, onnx, or ollama:MODEL@URL"
    ))
}

fn split_backend(spec: &str) -> Option<(&str, &str)> {
    const BACKENDS: &[&str] = &["local", "onnx", "ollama", "openai"];
    for backend in BACKENDS {
        if spec.eq_ignore_ascii_case(backend) {
            return Some((*backend, ""));
        }
        let mut prefix = String::from(*backend);
        prefix.push(':');
        if spec.len() > prefix.len() && spec[..prefix.len()].eq_ignore_ascii_case(&prefix) {
            return Some((*backend, &spec[prefix.len()..]));
        }
    }
    None
}

fn finish(
    backend: &str,
    model: Option<String>,
    url: Option<String>,
) -> Result<EmbedChoice, String> {
    match backend {
        "local" => {
            if model.is_some() || url.is_some() {
                return Err("local has no model and no url".into());
            }
            Ok(EmbedChoice::local())
        }
        "onnx" => {
            if model.is_some() || url.is_some() {
                return Err("onnx has no model and no url".into());
            }
            Ok(EmbedChoice::onnx())
        }
        "ollama" => Ok(EmbedChoice::ollama(model, url)),
        "openai" => Ok(EmbedChoice::openai(model, url)),
        _ => Err(format!("unknown embedding backend {backend:?}")),
    }
}

/// `ctxq.toml` at the workspace root, unless `.ctxq.toml` is the only file,
/// or `$CTXQ_CONFIG` / the caller names another path.
pub fn config_path(root: &Path, ctxq_config: Option<&Path>) -> std::path::PathBuf {
    if let Some(path) = ctxq_config {
        return path.to_path_buf();
    }
    let plain = root.join("ctxq.toml");
    let dot = root.join(".ctxq.toml");
    if !plain.is_file() && dot.is_file() {
        dot
    } else {
        plain
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedWrite {
    /// No file yet and the choice is the built-in local embedder.
    SkippedDefault,
    Unchanged,
    Written,
}

pub fn write_config(path: &Path, choice: &EmbedChoice) -> Result<EmbedWrite, String> {
    if !path.exists() && choice.is_default_local() {
        return Ok(EmbedWrite::SkippedDefault);
    }
    let original = if path.is_file() {
        Some(
            std::fs::read_to_string(path)
                .map_err(|e| format!("reading {}: {e}", path.display()))?,
        )
    } else {
        None
    };
    let (changed, text) = merge_embed(original.as_deref(), choice)?;
    if !changed {
        return Ok(EmbedWrite::Unchanged);
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
    }
    std::fs::write(path, text).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(EmbedWrite::Written)
}

/// Upserts `[embed]` and leaves every other table, comment and key alone.
/// `changed` is false when the file already states this choice.
pub fn merge_embed(original: Option<&str>, choice: &EmbedChoice) -> Result<(bool, String), String> {
    if let Some(text) = original {
        if !text.trim().is_empty() && matches_choice(text, choice) {
            return Ok((false, text.to_string()));
        }
    }
    let mut doc: DocumentMut = match original {
        Some(text) if !text.trim().is_empty() => {
            text.parse().map_err(|e| format!("invalid TOML: {e}"))?
        }
        _ => DocumentMut::new(),
    };
    {
        let table = embed_table(&mut doc)?;
        table["backend"] = toml_edit::value(choice.backend.clone());
        if let Some(model) = &choice.model {
            table["model"] = toml_edit::value(model.clone());
        }
        if let Some(url) = &choice.url {
            let mut urls = Array::new();
            urls.push(url.as_str());
            table["urls"] = Item::Value(urls.into());
        }
        if let Some(name) = &choice.api_key_env {
            table["api_key_env"] = toml_edit::value(name.clone());
        }
    }
    Ok((true, doc.to_string()))
}

fn embed_table(doc: &mut DocumentMut) -> Result<&mut Table, String> {
    if let Some(existing) = doc.get("embed") {
        if existing.is_inline_table() {
            return Err("[embed] is an inline table; expand it to a [embed] table".into());
        }
        if !existing.is_table() {
            return Err("[embed] exists and is not a table".into());
        }
    } else {
        doc["embed"] = Item::Table(Table::new());
    }
    doc["embed"]
        .as_table_mut()
        .ok_or_else(|| "[embed] exists and is not a table".into())
}

fn matches_choice(text: &str, choice: &EmbedChoice) -> bool {
    let Ok(value) = toml::from_str::<toml::Value>(text) else {
        return false;
    };
    let Some(embed) = value.get("embed") else {
        return false;
    };
    let backend = embed.get("backend").and_then(|v| v.as_str()).unwrap_or("local");
    if !backend.eq_ignore_ascii_case(&choice.backend) {
        return false;
    }
    if let Some(model) = &choice.model {
        if embed.get("model").and_then(|v| v.as_str()).unwrap_or("") != model {
            return false;
        }
    }
    if let Some(url) = &choice.url {
        let Some(urls) = embed.get("urls").and_then(|v| v.as_array()) else {
            return false;
        };
        if urls.len() != 1 || urls[0].as_str() != Some(url.as_str()) {
            return false;
        }
    }
    if let Some(name) = &choice.api_key_env {
        if embed.get("api_key_env").and_then(|v| v.as_str()) != Some(name.as_str()) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_and_qwen_alias() {
        assert_eq!(parse_embed_spec("ollama").unwrap(), EmbedChoice::ollama(None, None));
        assert_eq!(parse_embed_spec("qwen").unwrap().model.as_deref(), Some(OLLAMA_MODEL));
        assert_eq!(parse_embed_spec("2").unwrap().backend, "ollama");
        assert_eq!(
            parse_embed_spec("qwen3-embedding:0.6b").unwrap().model.as_deref(),
            Some(OLLAMA_MODEL)
        );
        let custom = parse_embed_spec("ollama:nomic-embed-text@http://gpu:11434/").unwrap();
        assert_eq!(custom.model.as_deref(), Some("nomic-embed-text"));
        assert_eq!(custom.url.as_deref(), Some("http://gpu:11434"));
        let hosted = parse_embed_spec("openai:text-embedding-3-small@https://api.openai.com/v1").unwrap();
        assert_eq!(hosted.api_key_env.as_deref(), Some("OPENAI_API_KEY"));
        assert!(parse_embed_spec("local:nope").is_err());
        assert!(parse_embed_spec("nope").is_err());
    }

    #[test]
    fn empty_menu_line_keeps_the_current_provider() {
        assert!(parse_embed_menu_line("  ").unwrap().is_none());
        assert_eq!(parse_embed_menu_line("qwen3").unwrap().unwrap().backend, "ollama");
        assert!(parse_embed_menu_line("nope").is_err());
    }

    #[test]
    fn merge_keeps_unrelated_keys() {
        let original = "\
# workspace
[search]
alpha = 0.2

[embed]
backend = \"local\"
max_chars = 6000

[embed.templates.nomic]
document = \"D: {t}\"
query = \"Q: {q}\"

[scan]
lang = \"fr\"
";
        let (changed, text) = merge_embed(Some(original), &EmbedChoice::ollama(None, None)).unwrap();
        assert!(changed);
        assert!(text.contains("alpha = 0.2"));
        assert!(text.contains("max_chars = 6000"));
        assert!(text.contains("lang = \"fr\""));
        assert!(text.contains("D: {t}"));
        assert!(text.contains("backend = \"ollama\""));
        assert!(text.contains("qwen3-embedding:0.6b"));
        let (again, same) = merge_embed(Some(&text), &EmbedChoice::ollama(None, None)).unwrap();
        assert!(!again);
        assert_eq!(same, text);
    }

    #[test]
    fn write_creates_then_skips_local_without_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ctxq.toml");
        assert_eq!(write_config(&path, &EmbedChoice::local()).unwrap(), EmbedWrite::SkippedDefault);
        assert!(!path.exists());
        assert_eq!(
            write_config(&path, &EmbedChoice::ollama(None, None)).unwrap(),
            EmbedWrite::Written
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[embed]"));
        assert_eq!(
            write_config(&path, &EmbedChoice::ollama(None, None)).unwrap(),
            EmbedWrite::Unchanged
        );
        let dot = dir.path().join(".ctxq.toml");
        std::fs::write(&dot, "[search]\nalpha = 0.2\n").unwrap();
        assert_eq!(config_path(dir.path(), None), path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(config_path(dir.path(), None), dot);
    }
}
