//! Intake mechanics, in the binary: intake inventory, conversion to a staging
//! area, repo pointers, and the context map (`INDEX.md`, `INDEX.json`,
//! `AGENTS.md`, `.gitignore`).
//!
//! `ingest` turns a heap of heterogeneous files dropped in an intake folder
//! into a `context/` tree that agents read through a one-line-per-doc map. The
//! *judgement* steps (classify, summarise) stay with the agent; this module
//! does the *mechanical* ones, with this on-disk layout:
//!
//! ```text
//! <context>/.organizer/inventory.json      what was found (hashes, duplicates, repos, secrets)
//! <context>/.organizer/mapping.json        sha256 → output (idempotence)
//! <context>/.organizer/conversion-log.json what happened to each file
//! <context>/.organizer/staging/*.md        converted docs with a front matter stub, to be classified
//! <context>/assets/                        binaries copied as-is
//! <context>/sources/<category>/*.md        classified docs (agent) + repo pointers
//! <context>/INDEX.md, INDEX.json, AGENTS.md, .gitignore
//! ```
//!
//! Compliance: a file carrying a secret is quarantined at inventory time and
//! never converted; converted text is scanned again before it is written.
//! Everything runs locally, nothing leaves the machine.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::core::convert;
use crate::core::docmeta;
use crate::core::secrets::SecretScanner;
use crate::core::settings::SecretsSettings;

// ------------------------------------------------------------------ classification

/// Extensions that go through the text pipeline (converted or read as text).
const TEXT_LIKE: &[&str] = &[
    "pptx", "docx", "pdf", "xlsx", "xls", "doc", "ppt", "md", "markdown", "txt", "rtf", "html",
    "htm", "csv", "json", "eml", "msg", "odt", "odp", "ods",
];
/// Binaries copied as-is to `assets/` (described, never converted).
const ASSET_LIKE: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "tiff", "fig", "sketch", "zip", "mp4", "mov",
];
/// Already textual: read directly as UTF-8 (no converter, robust to accents).
const PLAINTEXT: &[&str] = &[
    "md", "markdown", "txt", "csv", "tsv", "psv", "json", "yaml", "yml", "log", "rst",
];
/// Repo markers looked up in the intake's top-level folders.
const REPO_MARKERS: &[&str] = &[
    ".git",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "Cargo.toml",
];
/// Folders skipped when summarising a repo.
const REPO_IGNORE: &[&str] = &[
    ".git",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
    "target",
    ".next",
    ".cache",
    "vendor",
];
/// Only text files under this size are content-scanned for secrets at inventory time.
const SECRET_SCAN_MAX: u64 = 2 * 1024 * 1024;
const SECRET_SCAN_CHARS: usize = 200_000;

/// File names that denote key material or credentials (heuristic):
/// such a file is quarantined whatever its content.
static SECRET_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(service[-_]?account|credentials|secret|id_rsa|\.pem$|\.p12$|\.pfx$|(^|[-_.])env($|[-_.]))").unwrap()
});
/// Content patterns not covered by the index scanner.
static SECRET_EXTRA: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r#""private_key"\s*:"#, "gcp-service-account"),
        (r#""client_secret"\s*:"#, "oauth-client-secret"),
    ]
    .into_iter()
    .map(|(p, l)| (Regex::new(p).unwrap(), l))
    .collect()
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Text,
    Asset,
    Unknown,
}

fn kind_of(ext: &str) -> Kind {
    if TEXT_LIKE.contains(&ext) {
        Kind::Text
    } else if ASSET_LIKE.contains(&ext) {
        Kind::Asset
    } else {
        Kind::Unknown
    }
}

fn ext_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// `source_type` of the front matter schema, from the extension.
pub fn source_type_of(ext: &str) -> &'static str {
    match ext {
        "pptx" | "ppt" | "odp" => "pptx",
        "docx" | "doc" | "odt" | "rtf" => "docx",
        "pdf" => "pdf",
        "xlsx" | "xls" | "ods" | "csv" => "xlsx",
        "eml" | "msg" => "email",
        "html" | "htm" => "confluence",
        _ => "other",
    }
}

/// `Deck Kickoff V2.pptx` → `deck-kickoff-v2` (ASCII only).
pub fn slugify(name: &str) -> String {
    let base = match name.rfind('.') {
        Some(i) if i + 1 < name.len() => &name[..i],
        _ => name,
    };
    let mut out = String::new();
    let mut dash = false;
    for c in base.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "doc".into()
    } else {
        out
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h)?;
    Ok(format!("{:x}", h.finalize()))
}

fn looks_like_repo(d: &Path) -> bool {
    REPO_MARKERS.iter().any(|m| d.join(m).exists())
}

fn is_hidden(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false)
}

fn today() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let mut s = serde_json::to_string_pretty(value)?;
    s.push('\n');
    std::fs::write(path, s).with_context(|| format!("writing {}", path.display()))
}

fn read_json(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let s = std::fs::read_to_string(path)?;
    serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))
}

// ------------------------------------------------------------------ inventory

/// One file of the intake, as recorded in `inventory.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub path: String,
    pub name: String,
    pub suffix: String,
    pub kind: Kind,
    pub size: u64,
    pub mtime: String,
    pub date: String,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub secret: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_reason: Option<String>,
}

struct Scanner {
    index: SecretScanner,
}

impl Scanner {
    fn new() -> Self {
        Self {
            index: SecretScanner::new(&SecretsSettings::default())
                .unwrap_or_else(|_| SecretScanner::disabled()),
        }
    }

    /// Reason to quarantine `path` (`None` when clean). Name
    /// heuristic for everything, content patterns for text files under 2 MB.
    fn file(&self, path: &Path, kind: Kind, size: u64) -> Option<String> {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let by_name = || SECRET_NAME.is_match(name).then(|| format!("nom:{name}"));
        if kind != Kind::Text || size > SECRET_SCAN_MAX {
            return by_name();
        }
        let Ok(bytes) = std::fs::read(path) else {
            return by_name();
        };
        let text: String = String::from_utf8_lossy(&bytes)
            .chars()
            .take(SECRET_SCAN_CHARS)
            .collect();
        self.content(name, &text).or_else(by_name)
    }

    fn content(&self, name: &str, text: &str) -> Option<String> {
        self.index.scan(name, text).or_else(|| {
            SECRET_EXTRA
                .iter()
                .find(|(re, _)| re.is_match(text))
                .map(|(_, l)| (*l).to_string())
        })
    }
}

/// Inventory of the intake: files kept (duplicates by hash dropped, newest
/// kept), repos detected at the top level, secrets flagged.
pub fn inventory(intake: &Path, context: &Path) -> Result<Value> {
    if !intake.is_dir() {
        bail!("intake not found: {}", intake.display());
    }
    let organizer = context.join(".organizer");
    std::fs::create_dir_all(&organizer)?;
    let scanner = Scanner::new();

    let mut repos: Vec<String> = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(intake)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for child in &entries {
        if child.is_dir() && looks_like_repo(child) {
            repos.push(child.to_string_lossy().into_owned());
        }
    }
    let repo_paths: Vec<PathBuf> = repos
        .iter()
        .map(|r| std::fs::canonicalize(r).unwrap_or_else(|_| PathBuf::from(r)))
        .collect();

    let mut by_hash: HashMap<String, Record> = HashMap::new();
    let mut discarded: Vec<Value> = Vec::new();
    for entry in walkdir(intake, &repo_paths)? {
        let stat = entry.metadata()?;
        let mtime: DateTime<Utc> = stat.modified()?.into();
        let ext = ext_of(&entry);
        let kind = kind_of(&ext);
        let mut rec = Record {
            path: entry.to_string_lossy().into_owned(),
            name: entry
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string(),
            suffix: if ext.is_empty() {
                String::new()
            } else {
                format!(".{ext}")
            },
            kind,
            size: stat.len(),
            mtime: mtime.to_rfc3339_opts(SecondsFormat::Micros, false),
            date: mtime.format("%Y-%m-%d").to_string(),
            sha256: sha256_file(&entry)?,
            secret: false,
            secret_reason: None,
        };
        if let Some(reason) = scanner.file(&entry, kind, stat.len()) {
            rec.secret = true;
            rec.secret_reason = Some(reason);
        }
        match by_hash.get(&rec.sha256) {
            None => {
                by_hash.insert(rec.sha256.clone(), rec);
            }
            Some(prev) => {
                let (older, newer) = if rec.mtime < prev.mtime {
                    (rec, prev.clone())
                } else {
                    (prev.clone(), rec)
                };
                discarded.push(json!({ "dropped": older.path, "kept": newer.path, "reason": "duplicate-hash" }));
                by_hash.insert(newer.sha256.clone(), newer);
            }
        }
    }
    let mut kept: Vec<Record> = by_hash.into_values().collect();
    kept.sort_by(|a, b| a.path.cmp(&b.path));
    let secrets: Vec<&Record> = kept.iter().filter(|r| r.secret).collect();

    let inv = json!({
        "generated_at": now_iso(),
        "intake_dir": intake.to_string_lossy(),
        "context_dir": context.to_string_lossy(),
        "counts": {
            "kept": kept.len(),
            "discarded_duplicates": discarded.len(),
            "repos": repos.len(),
            "assets": kept.iter().filter(|r| r.kind == Kind::Asset).count(),
            "secrets": secrets.len(),
        },
        "files": kept,
        "discarded": discarded,
        "repos": repos,
    });
    write_json(&organizer.join("inventory.json"), &inv)?;
    Ok(inv)
}

/// Regular files under `root`, sorted, skipping hidden entries and the inside
/// of detected repos (summarised separately, never converted).
fn walkdir(root: &Path, repos: &[PathBuf]) -> Result<Vec<PathBuf>> {
    fn rec(dir: &Path, repos: &[PathBuf], out: &mut Vec<PathBuf>) -> Result<()> {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        entries.sort();
        for p in entries {
            if is_hidden(&p) {
                continue;
            }
            if p.is_dir() {
                let canon = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
                if repos.iter().any(|r| canon.starts_with(r)) {
                    continue;
                }
                rec(&p, repos, out)?;
            } else if p.is_file() {
                out.push(p);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    rec(root, repos, &mut out)?;
    Ok(out)
}

// ------------------------------------------------------------------ conversion

/// Front matter stub: the agent fills `category`, `summary`, `tags`, `title`.
fn front_matter_stub(rec: &Record) -> String {
    let q = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
    let ext = rec.suffix.trim_start_matches('.');
    [
        "---".to_string(),
        format!("title: {}", q(&rec.name)),
        format!("source_path: {}", q(&rec.path)),
        format!("source_type: {}", q(source_type_of(ext))),
        format!("date: {}", q(&rec.date)),
        "category: \"misc\"".to_string(),
        "project_phase: \"\"".to_string(),
        "summary: \"\"".to_string(),
        "tags: []".to_string(),
        "confidence: \"extracted\"".to_string(),
        "---".to_string(),
    ]
    .join("\n")
}

/// Markdown body of a text-like file, or the reason it cannot be converted.
fn convert_text(path: &Path, ext: &str, title: &str) -> Result<String> {
    if PLAINTEXT.contains(&ext) {
        let bytes = std::fs::read(path)?;
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }
    if convert::is_convertible(ext) {
        let bytes = std::fs::read(path)?;
        return convert::to_markdown(ext, &bytes, title);
    }
    match ext {
        "html" | "htm" => Ok(html_to_text(&String::from_utf8_lossy(&std::fs::read(
            path,
        )?))),
        "eml" => Ok(eml_to_markdown(&std::fs::read(path)?)),
        other => bail!("no converter for .{other} (fallback: MarkItDown)"),
    }
}

/// Tags stripped, block elements turned into line breaks, common entities decoded.
pub fn html_to_text(html: &str) -> String {
    static SCRIPT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<(script|style)[^>]*>.*?</(script|style)>").unwrap());
    static BLOCK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)</?(p|div|br|h[1-6]|li|tr|table|section|article|blockquote|pre)[^>]*>")
            .unwrap()
    });
    static H: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<h([1-6])[^>]*>(.*?)</h[1-6]>").unwrap());
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]+>").unwrap());
    static BLANKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{3,}").unwrap());
    let s = SCRIPT.replace_all(html, "");
    let s = H.replace_all(&s, |c: &regex::Captures| {
        let level: usize = c[1].parse().unwrap_or(1);
        format!(
            "\n{} {}\n",
            "#".repeat(level),
            TAG.replace_all(&c[2], "").trim()
        )
    });
    let s = BLOCK.replace_all(&s, "\n");
    let s = TAG.replace_all(&s, "");
    let s = s
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'");
    let lines: Vec<String> = s.lines().map(|l| l.trim().to_string()).collect();
    BLANKS
        .replace_all(lines.join("\n").trim(), "\n\n")
        .into_owned()
}

/// Headers of interest plus the first `text/plain` body (quoted-printable and
/// base64 decoded). Good enough for exported threads; attachments are dropped.
pub fn eml_to_markdown(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw).replace("\r\n", "\n");
    let (head, body) = text.split_once("\n\n").unwrap_or((text.as_str(), ""));
    let headers = unfold_headers(head);
    let get = |k: &str| {
        headers
            .get(&k.to_ascii_lowercase())
            .cloned()
            .unwrap_or_default()
    };
    let mut out = String::new();
    for (label, key) in [
        ("From", "from"),
        ("To", "to"),
        ("Cc", "cc"),
        ("Date", "date"),
        ("Subject", "subject"),
    ] {
        let v = get(key);
        if !v.is_empty() {
            out.push_str(&format!("- **{label}**: {v}\n"));
        }
    }
    out.push('\n');
    let ctype = get("content-type");
    let body_text = match boundary_of(&ctype) {
        Some(b) => mime_text_part(body, &b).unwrap_or_default(),
        None => decode_transfer(body, &get("content-transfer-encoding"), &ctype),
    };
    out.push_str(body_text.trim());
    out.push('\n');
    out
}

fn unfold_headers(head: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut current: Option<(String, String)> = None;
    for line in head.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(cur) = current.as_mut() {
                cur.1.push_str(line.trim());
                continue;
            }
        }
        if let Some((k, v)) = current.take() {
            map.entry(k).or_insert(v);
        }
        if let Some((k, v)) = line.split_once(':') {
            current = Some((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    if let Some((k, v)) = current {
        map.entry(k).or_insert(v);
    }
    map
}

fn boundary_of(ctype: &str) -> Option<String> {
    static B: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?i)boundary="?([^";]+)"?"#).unwrap());
    if !ctype.to_ascii_lowercase().contains("multipart/") {
        return None;
    }
    B.captures(ctype).map(|c| c[1].to_string())
}

/// Depth-first search of the first `text/plain` part (multipart parts recurse).
fn mime_text_part(body: &str, boundary: &str) -> Option<String> {
    let sep = format!("--{boundary}");
    let mut html_fallback = None;
    for part in body.split(&sep).skip(1) {
        if part.starts_with("--") {
            break;
        }
        let part = part.trim_start_matches('\n');
        let (head, content) = part.split_once("\n\n").unwrap_or((part, ""));
        let headers = unfold_headers(head);
        let ctype = headers
            .get("content-type")
            .cloned()
            .unwrap_or_else(|| "text/plain".into());
        let lc = ctype.to_ascii_lowercase();
        if let Some(b) = boundary_of(&ctype) {
            if let Some(t) = mime_text_part(content, &b) {
                return Some(t);
            }
        } else if lc.starts_with("text/plain") {
            return Some(decode_transfer(
                content,
                headers
                    .get("content-transfer-encoding")
                    .map_or("", String::as_str),
                &ctype,
            ));
        } else if lc.starts_with("text/html") && html_fallback.is_none() {
            let decoded = decode_transfer(
                content,
                headers
                    .get("content-transfer-encoding")
                    .map_or("", String::as_str),
                &ctype,
            );
            html_fallback = Some(html_to_text(&decoded));
        }
    }
    html_fallback
}

fn decode_transfer(content: &str, encoding: &str, _ctype: &str) -> String {
    match encoding.trim().to_ascii_lowercase().as_str() {
        "quoted-printable" => decode_qp(content),
        "base64" => {
            let bytes = decode_base64(content);
            String::from_utf8_lossy(&bytes).into_owned()
        }
        _ => content.to_string(),
    }
}

fn decode_qp(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                i += 2; // soft line break
                continue;
            }
            if i + 2 < bytes.len() {
                if let Ok(v) = u8::from_str_radix(
                    std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"),
                    16,
                ) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn decode_base64(s: &str) -> Vec<u8> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a') as u32 + 26),
            b'0'..=b'9' => Some((c - b'0') as u32 + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let Some(v) = val(c) else { continue };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    out
}

/// Result of one `ingest` run (numbers for the summary line, paths for the agent).
#[derive(Debug, Default, Serialize)]
pub struct IngestReport {
    pub kept: usize,
    pub discarded_duplicates: usize,
    pub repos: usize,
    pub assets: usize,
    pub secrets: usize,
    pub converted: usize,
    pub copied: usize,
    pub skipped: usize,
    pub quarantined: usize,
    pub failed: usize,
    /// Files that could not be converted, with the reason.
    pub failures: Vec<(String, String)>,
    /// Quarantined files, with the kind of secret (never the secret).
    pub quarantine: Vec<(String, String)>,
    pub repo_pointers: Vec<String>,
    pub staging: PathBuf,
    pub inventory: PathBuf,
}

pub struct IngestOptions {
    pub intake: PathBuf,
    pub context: PathBuf,
    /// Re-convert files already present in `mapping.json`.
    pub force: bool,
}

/// Steps 1 and 2 (+ repo pointers): inventory, then convert every kept
/// text file to `staging/`, copy assets, write one pointer per repo.
pub fn ingest(opts: &IngestOptions) -> Result<IngestReport> {
    let inv = inventory(&opts.intake, &opts.context)?;
    let organizer = opts.context.join(".organizer");
    let staging = organizer.join("staging");
    let assets_dir = opts.context.join("assets");
    std::fs::create_dir_all(&staging)?;
    std::fs::create_dir_all(&assets_dir)?;
    let mapping_path = organizer.join("mapping.json");
    let mut mapping = read_json(&mapping_path)?;
    let scanner = Scanner::new();

    let mut report = IngestReport {
        kept: inv["counts"]["kept"].as_u64().unwrap_or(0) as usize,
        discarded_duplicates: inv["counts"]["discarded_duplicates"].as_u64().unwrap_or(0) as usize,
        repos: inv["counts"]["repos"].as_u64().unwrap_or(0) as usize,
        assets: inv["counts"]["assets"].as_u64().unwrap_or(0) as usize,
        secrets: inv["counts"]["secrets"].as_u64().unwrap_or(0) as usize,
        staging: staging.clone(),
        inventory: organizer.join("inventory.json"),
        ..Default::default()
    };
    let mut log: Vec<Value> = Vec::new();
    let files: Vec<Record> = serde_json::from_value(inv["files"].clone())?;

    for rec in &files {
        let done = mapping.get(&rec.sha256).is_some();
        if done && !opts.force {
            report.skipped += 1;
            continue;
        }
        if rec.secret {
            report.quarantined += 1;
            let reason = rec.secret_reason.clone().unwrap_or_default();
            log.push(json!({ "source": rec.path, "quarantined": reason }));
            report.quarantine.push((rec.path.clone(), reason));
            continue;
        }
        let src = Path::new(&rec.path);
        match rec.kind {
            Kind::Asset => {
                let dest = assets_dir.join(format!("{}{}", slugify(&rec.name), rec.suffix));
                std::fs::copy(src, &dest).with_context(|| format!("copying {}", src.display()))?;
                mapping[&rec.sha256] = json!({ "output": dest.to_string_lossy(), "kind": "asset", "source": rec.path });
                report.copied += 1;
                log.push(json!({ "source": rec.path, "output": dest.to_string_lossy(), "kind": "asset" }));
            }
            Kind::Unknown => {
                log.push(json!({ "source": rec.path, "skipped": "type inconnu" }));
            }
            Kind::Text => {
                let ext = rec.suffix.trim_start_matches('.');
                let title = src
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let body = match convert_text(src, ext, &title) {
                    Ok(b) => b,
                    Err(e) => {
                        report.failed += 1;
                        log.push(json!({ "source": rec.path, "error": e.to_string() }));
                        report.failures.push((rec.path.clone(), e.to_string()));
                        continue;
                    }
                };
                // Second line of defence: a key inside an office document is
                // only visible once converted.
                if let Some(kind) = scanner.content(&rec.name, &body) {
                    report.quarantined += 1;
                    log.push(json!({ "source": rec.path, "quarantined": kind }));
                    report.quarantine.push((rec.path.clone(), kind));
                    continue;
                }
                let previous = mapping
                    .get(&rec.sha256)
                    .and_then(|m| m["output"].as_str())
                    .map(PathBuf::from);
                let out_path = staging_name(
                    &staging,
                    &rec.date,
                    &slugify(&rec.name),
                    previous.as_deref(),
                );
                let content = format!("{}\n\n{}\n", front_matter_stub(rec), body.trim());
                std::fs::write(&out_path, content)?;
                mapping[&rec.sha256] = json!({ "output": out_path.to_string_lossy(), "kind": "text", "source": rec.path });
                report.converted += 1;
                log.push(json!({ "source": rec.path, "output": out_path.to_string_lossy(), "kind": "text" }));
            }
        }
    }

    for repo in inv["repos"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let key = format!("repo:{repo}");
        if mapping.get(&key).is_some() && !opts.force {
            report.skipped += 1;
            continue;
        }
        let out = repo_pointer(Path::new(repo), &opts.context)?;
        mapping[&key] = json!({ "output": out.to_string_lossy(), "kind": "repo", "source": repo });
        log.push(json!({ "source": repo, "output": out.to_string_lossy(), "kind": "repo" }));
        report
            .repo_pointers
            .push(out.to_string_lossy().into_owned());
    }

    write_json(&mapping_path, &mapping)?;
    write_json(
        &organizer.join("conversion-log.json"),
        &json!({ "at": now_iso(), "entries": log }),
    )?;
    Ok(report)
}

/// `<staging>/<date>_<slug>.md`, suffixed `-2`, `-3`… when another source
/// already took the name (a re-conversion keeps its own file).
fn staging_name(staging: &Path, date: &str, slug: &str, previous: Option<&Path>) -> PathBuf {
    let date = if date.is_empty() { "0000-00-00" } else { date };
    let mut out = staging.join(format!("{date}_{slug}.md"));
    let mut n = 2;
    while out.exists() && previous != Some(out.as_path()) {
        out = staging.join(format!("{date}_{slug}-{n}.md"));
        n += 1;
    }
    out
}

// ------------------------------------------------------------------ repo pointer

const LANG_BY_EXT: &[(&str, &str)] = &[
    ("py", "Python"),
    ("js", "JavaScript"),
    ("ts", "TypeScript"),
    ("tsx", "TypeScript"),
    ("jsx", "JavaScript"),
    ("go", "Go"),
    ("rs", "Rust"),
    ("java", "Java"),
    ("rb", "Ruby"),
    ("php", "PHP"),
    ("cs", "C#"),
    ("cpp", "C++"),
    ("c", "C"),
    ("kt", "Kotlin"),
    ("swift", "Swift"),
    ("sql", "SQL"),
    ("sh", "Shell"),
    ("md", "Markdown"),
    ("yml", "YAML"),
    ("yaml", "YAML"),
    ("json", "JSON"),
    ("html", "HTML"),
    ("css", "CSS"),
];

fn repo_tree(root: &Path, max_depth: usize, max_entries: usize) -> Vec<String> {
    fn walk(
        d: &Path,
        prefix: &str,
        depth: usize,
        max_depth: usize,
        max_entries: usize,
        lines: &mut Vec<String>,
        count: &mut usize,
    ) {
        if depth > max_depth || *count >= max_entries {
            return;
        }
        let Ok(rd) = std::fs::read_dir(d) else { return };
        let mut entries: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                !REPO_IGNORE.contains(&name) && !name.starts_with('.')
            })
            .collect();
        entries.sort_by_key(|p| {
            (
                p.is_file(),
                p.file_name().map(|n| n.to_string_lossy().to_lowercase()),
            )
        });
        for e in entries {
            if *count >= max_entries {
                lines.push(format!("{prefix}… (truncated)"));
                return;
            }
            let name = e
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let marker = if e.is_dir() { "/" } else { "" };
            lines.push(format!("{prefix}{name}{marker}"));
            *count += 1;
            if e.is_dir() {
                walk(
                    &e,
                    &format!("{prefix}  "),
                    depth + 1,
                    max_depth,
                    max_entries,
                    lines,
                    count,
                );
            }
        }
    }
    let mut lines = Vec::new();
    let mut count = 0;
    walk(root, "", 1, max_depth, max_entries, &mut lines, &mut count);
    lines
}

/// A repo dropped in the intake is not converted (agents grep the code): one
/// pointer document in `sources/misc/` gives its path, size, languages, tree
/// and README head.
pub fn repo_pointer(repo: &Path, context: &Path) -> Result<PathBuf> {
    if !repo.is_dir() {
        bail!("repo not found: {}", repo.display());
    }
    let mut langs: BTreeMap<&str, usize> = BTreeMap::new();
    let mut nfiles = 0usize;
    fn count(d: &Path, nfiles: &mut usize, langs: &mut BTreeMap<&str, usize>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if p.is_dir() {
                if !REPO_IGNORE.contains(&name) {
                    count(&p, nfiles, langs);
                }
            } else if p.is_file() {
                *nfiles += 1;
                let ext = ext_of(&p);
                if let Some((_, lang)) = LANG_BY_EXT.iter().find(|(e, _)| *e == ext) {
                    *langs.entry(lang).or_default() += 1;
                }
            }
        }
    }
    count(repo, &mut nfiles, &mut langs);
    let mut top: Vec<(&str, usize)> = langs.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let lang_str = if top.is_empty() {
        "—".to_string()
    } else {
        top.iter()
            .take(8)
            .map(|(l, n)| format!("{l} ({n})"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let readme = ["README.md", "README", "readme.md"]
        .iter()
        .find_map(|c| std::fs::read_to_string(repo.join(c)).ok());
    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let today = today();
    let out_dir = context.join("sources").join("misc");
    std::fs::create_dir_all(&out_dir)?;
    let out_path = out_dir.join(format!(
        "{today}_repo_{}.md",
        name.to_lowercase().replace(' ', "-")
    ));
    let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
    let summary = format!(
        "Pointer to repo {name} ({nfiles} files, languages: {lang_str}). The code is not converted: grep the source directly."
    );
    let mut parts = vec![
        "---".to_string(),
        format!("title: {}", q(&format!("Repo — {name}"))),
        format!("source_path: {}", q(&repo.to_string_lossy())),
        "source_type: repo".to_string(),
        format!("date: {today}"),
        "category: misc".to_string(),
        "project_phase: pure-code".to_string(),
        format!("summary: {}", q(&summary)),
        "tags: [repo, code]".to_string(),
        "confidence: extracted".to_string(),
        "---".to_string(),
        String::new(),
        format!("# Repo — {name}"),
        String::new(),
        format!("- **Source path**: `{}`", repo.display()),
        format!("- **Files** (excluding deps): {nfiles}"),
        format!("- **Languages**: {lang_str}"),
        String::new(),
        "> ⚠️ This code is not converted to markdown. For any information, **grep the source directly** at the path above.".to_string(),
        String::new(),
        "## Directory tree (summary)".to_string(),
        String::new(),
        "```".to_string(),
    ];
    parts.extend(repo_tree(repo, 3, 200));
    parts.push("```".to_string());
    if let Some(r) = readme {
        let head: Vec<&str> = r.lines().take(40).collect();
        parts.extend([
            "".into(),
            "## README excerpt".into(),
            "".into(),
            "```markdown".into(),
            head.join("\n"),
            "```".into(),
        ]);
    }
    std::fs::write(&out_path, parts.join("\n") + "\n")?;
    Ok(out_path)
}

// ------------------------------------------------------------------ map

const CATEGORY_ORDER: &[(&str, &str)] = &[
    ("decisions", "Decisions (ADR)"),
    ("specs", "Specs & scoping"),
    ("meetings", "Meetings / minutes"),
    ("mails", "Mails"),
    ("design", "Design"),
    ("misc", "Misc"),
];

/// The `AGENTS.md` template (placeholders `{{PROFILE}}`, `{{INTAKE}}`,
/// `{{DATE}}`, `{{DOC_COUNT}}`); `MapOptions::agents_template` overrides it.
pub const AGENTS_TEMPLATE: &str = include_str!("../cli/templates/intake-agents.md.tmpl");

fn category_label(cat: &str) -> String {
    CATEGORY_ORDER
        .iter()
        .find(|(k, _)| *k == cat)
        .map(|(_, l)| (*l).to_string())
        .unwrap_or_else(|| {
            let mut c = cat.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
}

pub struct MapOptions {
    pub context: PathBuf,
    pub profile: String,
    /// Intake folder name, for `.gitignore` and `AGENTS.md`.
    pub intake: String,
    /// Build even when `staging/` still holds unclassified documents.
    pub force: bool,
    pub agents_template: Option<PathBuf>,
}

#[derive(Debug, Default, Serialize)]
pub struct MapReport {
    pub docs: usize,
    pub categories: usize,
    /// Unclassified documents left in `staging/` (the map is refused unless forced).
    pub leftover_staging: Vec<String>,
    /// Text conversions recorded in `mapping.json`, to cross-check against `docs`.
    pub conversions: Option<usize>,
    pub written: bool,
    pub index_md: PathBuf,
}

#[derive(Debug, Serialize)]
struct MapDoc {
    title: String,
    category: String,
    date: String,
    summary: String,
    tags: Vec<String>,
    path: String,
}

fn md_files(dir: &Path) -> Vec<PathBuf> {
    fn rec(d: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                rec(&p, out);
            } else if ext_of(&p) == "md" {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    rec(dir, &mut out);
    out.sort();
    out
}

/// Step 5: `INDEX.md` (the map), `INDEX.json`, `AGENTS.md`,
/// `.gitignore` from the front matter of `sources/**/*.md`. Refuses to build
/// while `staging/` still holds documents (each would be a silently lost doc).
pub fn export_map(opts: &MapOptions) -> Result<MapReport> {
    let sources = opts.context.join("sources");
    if !sources.is_dir() {
        bail!("{} not found", sources.display());
    }
    let mut report = MapReport {
        index_md: opts.context.join("INDEX.md"),
        ..Default::default()
    };

    let staging = opts.context.join(".organizer").join("staging");
    report.leftover_staging = md_files(&staging)
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let mapping = read_json(&opts.context.join(".organizer").join("mapping.json"))?;
    if let Some(m) = mapping.as_object() {
        if !m.is_empty() {
            report.conversions = Some(m.values().filter(|v| v["kind"] == "text").count());
        }
    }
    if !report.leftover_staging.is_empty() && !opts.force {
        return Ok(report);
    }

    let mut docs: Vec<MapDoc> = Vec::new();
    for path in md_files(&sources) {
        let content = std::fs::read_to_string(&path)?;
        let rel = path
            .strip_prefix(&opts.context)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        let meta = docmeta::from_content(&rel, &content);
        docs.push(MapDoc {
            title: meta.title.unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            }),
            category: meta.category.unwrap_or_else(|| {
                path.parent()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            }),
            date: meta.date.unwrap_or_default(),
            summary: meta.summary.unwrap_or_default(),
            tags: meta.tags,
            path: rel,
        });
    }

    let mut by_cat: BTreeMap<String, Vec<&MapDoc>> = BTreeMap::new();
    for d in &docs {
        by_cat.entry(d.category.clone()).or_default().push(d);
    }
    let mut ordered: Vec<String> = CATEGORY_ORDER
        .iter()
        .map(|(k, _)| k.to_string())
        .filter(|k| by_cat.contains_key(k))
        .collect();
    ordered.extend(
        by_cat
            .keys()
            .filter(|k| !CATEGORY_ORDER.iter().any(|(c, _)| c == k))
            .cloned(),
    );
    let today = today();

    let mut lines: Vec<String> = vec![
        "# INDEX — project context map".into(),
        String::new(),
        format!(
            "> **{} document(s)** · profile **`{}`** · last organized **{today}**",
            docs.len(),
            opts.profile
        ),
        ">".into(),
        "> Read this map first. Open only the documents your task needs.".into(),
        String::new(),
    ];
    for cat in &ordered {
        let mut items: Vec<&MapDoc> = by_cat[cat].clone();
        items.sort_by(|a, b| {
            let da = if a.date.is_empty() { "0" } else { &a.date };
            let db = if b.date.is_empty() { "0" } else { &b.date };
            da.cmp(db).then(a.title.cmp(&b.title))
        });
        lines.push(format!("## {} ({})", category_label(cat), items.len()));
        lines.push(String::new());
        for d in items {
            let date = if d.date.is_empty() {
                "*—*".to_string()
            } else {
                format!("*{}*", d.date)
            };
            let summary = if d.summary.is_empty() {
                "_(summary missing)_".to_string()
            } else {
                d.summary.clone()
            };
            let tags = if d.tags.is_empty() {
                String::new()
            } else {
                format!(
                    " `{}`",
                    d.tags
                        .iter()
                        .map(|t| format!("#{t}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            };
            lines.push(format!(
                "- [{}]({}) — {date} — {summary}{tags}",
                d.title, d.path
            ));
        }
        lines.push(String::new());
    }
    std::fs::write(&report.index_md, lines.join("\n"))?;

    let mut sorted: Vec<&MapDoc> = docs.iter().collect();
    sorted.sort_by(|a, b| {
        let da = if a.date.is_empty() { "0" } else { &a.date };
        let db = if b.date.is_empty() { "0" } else { &b.date };
        a.category
            .cmp(&b.category)
            .then(da.cmp(db))
            .then(a.title.cmp(&b.title))
    });
    let index_json = json!({
        "generated_at": now_iso(),
        "profile": opts.profile,
        "intake": opts.intake,
        "count": docs.len(),
        "categories": ordered.iter().map(|c| json!({ "key": c, "label": category_label(c), "count": by_cat[c].len() })).collect::<Vec<_>>(),
        "docs": sorted,
    });
    write_json(&opts.context.join("INDEX.json"), &index_json)?;

    let template = match &opts.agents_template {
        Some(p) => {
            std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?
        }
        None => AGENTS_TEMPLATE.to_string(),
    };
    let agents = template
        .replace("{{PROFILE}}", &opts.profile)
        .replace("{{INTAKE}}", &opts.intake)
        .replace("{{DATE}}", &today)
        .replace("{{DOC_COUNT}}", &docs.len().to_string());
    std::fs::write(opts.context.join("AGENTS.md"), agents)?;

    let gitignore = format!(
        "# documentum ingest — never push potentially confidential content\n/{}/\nsources/\nassets/\n.organizer/\n",
        opts.intake
    );
    std::fs::write(opts.context.join(".gitignore"), gitignore)?;

    report.docs = docs.len();
    report.categories = ordered.len();
    report.written = true;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_is_ascii_only() {
        assert_eq!(slugify("Deck Kickoff V2.pptx"), "deck-kickoff-v2");
        assert_eq!(slugify("mon fichiertest.md"), "mon-fichiertest");
        assert_eq!(slugify("Réunion été.docx"), "r-union-t");
        assert_eq!(slugify("...."), "doc");
        assert_eq!(slugify(".env"), "doc");
    }

    #[test]
    fn html_strips_tags_and_keeps_structure() {
        let md = html_to_text("<html><head><style>p{}</style></head><body><h1>Titre</h1><p>Un &amp; deux</p><ul><li>a</li><li>b</li></ul></body></html>");
        assert!(md.starts_with("# Titre"), "{md}");
        assert!(md.contains("Un & deux"));
        assert!(md.contains("a\nb") || md.contains("a\n\nb"), "{md}");
    }

    #[test]
    fn eml_headers_and_quoted_printable_body() {
        let raw = "From: a@x.fr\nTo: b@y.fr\nSubject: Point\n archi\nContent-Type: text/plain; charset=utf-8\nContent-Transfer-Encoding: quoted-printable\n\nD=C3=A9cision prise =\nhier.\n";
        let md = eml_to_markdown(raw.as_bytes());
        assert!(md.contains("**Subject**: Pointarchi"), "{md}");
        assert!(md.contains("Décision prise hier."), "{md}");
    }

    #[test]
    fn eml_multipart_prefers_text_plain() {
        let raw = "Subject: S\nContent-Type: multipart/alternative; boundary=\"B\"\n\n--B\nContent-Type: text/html\n\n<p>html</p>\n--B\nContent-Type: text/plain\nContent-Transfer-Encoding: base64\n\nQm9uam91cg==\n--B--\n";
        let md = eml_to_markdown(raw.as_bytes());
        assert!(md.trim().ends_with("Bonjour"), "{md}");
    }

    #[test]
    fn secret_name_heuristic() {
        let s = Scanner::new();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("service-account.json");
        std::fs::write(&p, "{}").unwrap();
        assert_eq!(
            s.file(&p, Kind::Text, 2).as_deref(),
            Some("nom:service-account.json")
        );
        let q = dir.path().join("notes.md");
        std::fs::write(&q, "rien").unwrap();
        assert!(s.file(&q, Kind::Text, 4).is_none());
        let r = dir.path().join("sa.json");
        std::fs::write(&r, r#"{"type": "x", "private_key": "abc"}"#).unwrap();
        assert_eq!(
            s.file(&r, Kind::Text, 40).as_deref(),
            Some("gcp-service-account")
        );
    }
}
