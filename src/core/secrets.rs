//! Secret detection before indexing.
//!
//! An index is served to agents: a key that gets indexed is a key that ends up
//! in a prompt. A file with a high-confidence secret is therefore quarantined —
//! never chunked, never embedded, its previous chunks dropped — and listed with
//! the *kind* of secret found, never the secret itself.
//!
//! Patterns are deliberately narrow (provider-specific token shapes, key
//! blocks): a false positive hides a whole document. Configuration values are
//! handled separately by key-based redaction (`configs::redact_secret_value`).

use std::path::Path;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use globset::GlobSet;
use regex::Regex;

use crate::core::settings::SecretsSettings;

static BUILTIN: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"-----BEGIN [A-Z ]*PRIVATE KEY-----", "private-key-block"),
        (r#""private_key"\s*:\s*"-----BEGIN"#, "gcp-service-account"),
        (r"\bAKIA[0-9A-Z]{16}\b", "aws-access-key"),
        (r"\bxox[baprs]-[0-9A-Za-z-]{10,}", "slack-token"),
        (r"\bgh[pousr]_[0-9A-Za-z]{36,}", "github-token"),
        (r"\bgithub_pat_[0-9A-Za-z_]{60,}", "github-token"),
        (r"\bglpat-[0-9A-Za-z_-]{20,}", "gitlab-token"),
        (r"\bsk-ant-[0-9A-Za-z_-]{40,}", "anthropic-key"),
        (r"\bsk-(?:proj-)?[0-9A-Za-z_-]{40,}", "openai-key"),
        (r"\b[sr]k_live_[0-9A-Za-z]{20,}", "stripe-live-key"),
        (r"\bAIza[0-9A-Za-z_-]{35}\b", "google-api-key"),
    ]
    .into_iter()
    .map(|(p, l)| (Regex::new(p).expect("builtin secret pattern"), l))
    .collect()
});

/// Key material recognised by file name alone.
static KEY_FILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\.(pem|p12|pfx|key|keystore|jks)$|^id_(rsa|dsa|ecdsa|ed25519)$)").unwrap()
});

pub struct SecretScanner {
    enabled: bool,
    extra: Vec<Regex>,
    allow: GlobSet,
}

impl SecretScanner {
    pub fn new(settings: &SecretsSettings) -> Result<Self> {
        let extra = settings
            .patterns
            .iter()
            .map(|p| Regex::new(p).with_context(|| format!("[secrets] pattern '{p}'")))
            .collect::<Result<_>>()?;
        Ok(Self {
            enabled: settings.enabled,
            extra,
            allow: crate::core::collect::globset(&settings.allow)?,
        })
    }

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            extra: Vec::new(),
            allow: GlobSet::empty(),
        }
    }

    /// Kind of secret found in `content` (or the file name), if any.
    pub fn scan(&self, rel_path: &str, content: &str) -> Option<String> {
        if !self.enabled || self.allow.is_match(rel_path) {
            return None;
        }
        let name = Path::new(rel_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if KEY_FILE.is_match(name) {
            return Some("key-file".into());
        }
        if let Some((_, label)) = BUILTIN.iter().find(|(re, _)| re.is_match(content)) {
            return Some((*label).into());
        }
        self.extra
            .iter()
            .position(|re| re.is_match(content))
            .map(|i| format!("custom-pattern-{}", i + 1))
    }
}

/// Scanner of the workspace rooted at `root` (its `[secrets]` settings), cached
/// per root: code paths read sources on demand and must not re-parse the
/// configuration for every symbol body. An unreadable configuration falls back
/// to the defaults, which scan — failing closed.
pub fn scanner_for(root: &Path) -> std::sync::Arc<SecretScanner> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    static CACHE: LazyLock<Mutex<HashMap<std::path::PathBuf, Arc<SecretScanner>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let key = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut cache = CACHE.lock().unwrap();
    cache
        .entry(key)
        .or_insert_with(|| {
            let settings = crate::core::settings::Settings::load(root)
                .map(|s| s.secrets)
                .unwrap_or_default();
            Arc::new(SecretScanner::new(&settings).unwrap_or_else(|e| {
                tracing::warn!("[secrets] settings unusable ({e:#}); using defaults");
                SecretScanner::new(&SecretsSettings::default()).expect("default scanner")
            }))
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scanner() -> SecretScanner {
        SecretScanner::new(&SecretsSettings::default()).unwrap()
    }

    #[test]
    fn flags_high_confidence_tokens_only() {
        let s = scanner();
        let gh = format!("token: ghp_{}", "a".repeat(36));
        assert_eq!(s.scan("notes/x.md", &gh).as_deref(), Some("github-token"));
        assert_eq!(
            s.scan("a.md", "-----BEGIN OPENSSH PRIVATE KEY-----\nabc")
                .as_deref(),
            Some("private-key-block")
        );
        assert_eq!(s.scan("keys/server.pem", "").as_deref(), Some("key-file"));
        // Prose about secrets is not a secret.
        assert_eq!(
            s.scan("docs/secrets-migration.md", "We store the API key in SOPS."),
            None
        );
        assert_eq!(s.scan("a.md", "AKIA is the prefix of AWS keys"), None);
    }

    #[test]
    fn honours_allow_list_custom_patterns_and_switch() {
        let s = SecretScanner::new(&SecretsSettings {
            enabled: true,
            patterns: vec![r"\bMYCO-[0-9]{8}\b".into()],
            allow: vec!["fixtures/**".into()],
        })
        .unwrap();
        assert_eq!(
            s.scan("a.md", "id MYCO-12345678").as_deref(),
            Some("custom-pattern-1")
        );
        assert_eq!(s.scan("fixtures/k.pem", ""), None);
        assert_eq!(SecretScanner::disabled().scan("k.pem", ""), None);
    }
}
