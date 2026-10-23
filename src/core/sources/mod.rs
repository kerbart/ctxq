//! Inventory sources: each `[[scan.source]]` produces one dated markdown
//! document under `[scan].output_dir`, indexed like any other file (its tables
//! row by row).
//!
//! The core knows no service: `kind = "command"` runs whatever is declared.
//! The other kinds are optional built-ins for what most Linux hosts share
//! (Docker, systemd, listening ports, project and script directories), so a
//! single binary is enough without helper scripts.

mod builtins;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::core::settings::{Settings, SourceFormat, SourceSettings};

/// Generated-prose vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Fr,
}

impl Lang {
    pub fn parse(s: &str) -> Self {
        if s.eq_ignore_ascii_case("fr") {
            Self::Fr
        } else {
            Self::En
        }
    }

    pub(crate) fn t(self, en: &'static str, fr: &'static str) -> &'static str {
        match self {
            Self::En => en,
            Self::Fr => fr,
        }
    }
}

/// What a collector returns: a title, an intro paragraph, and the body.
pub struct Section {
    pub title: String,
    pub intro: Option<String>,
    pub body: String,
}

#[derive(Debug)]
pub struct ScanReport {
    pub name: String,
    pub path: PathBuf,
    pub changed: bool,
    pub error: Option<String>,
}

pub(crate) fn opt_str<'a>(src: &'a SourceSettings, key: &str) -> Option<&'a str> {
    src.options.get(key).and_then(|v| v.as_str())
}

pub(crate) fn opt_list(src: &SourceSettings, key: &str) -> Option<Vec<String>> {
    src.options.get(key).and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect()
    })
}

pub(crate) fn opt_bool(src: &SourceSettings, key: &str) -> Option<bool> {
    src.options.get(key).and_then(|v| v.as_bool())
}

/// Runs `argv` in `cwd` with a timeout; stdout on success.
pub fn run_command(argv: &[String], cwd: &Path, timeout: Duration) -> Result<String> {
    run(argv, cwd, timeout)
}

pub(crate) fn run(argv: &[String], cwd: &Path, timeout: Duration) -> Result<String> {
    use std::io::Read;
    let Some((prog, args)) = argv.split_first() else {
        bail!("empty command");
    };
    let mut child = std::process::Command::new(prog)
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {prog}"))?;
    let mut out = child.stdout.take().expect("piped stdout");
    let mut err = child.stderr.take().expect("piped stderr");
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let start = std::time::Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{prog} timed out after {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    if !status.success() {
        bail!("{prog} exited with {status}: {}", stderr.trim());
    }
    Ok(stdout)
}

fn collect(src: &SourceSettings, root: &Path, lang: Lang) -> Result<Section> {
    let timeout = Duration::from_secs(
        src.options
            .get("timeout_secs")
            .and_then(|v| v.as_integer())
            .unwrap_or(60)
            .max(1) as u64,
    );
    match src.kind.as_str() {
        "command" => {
            if src.command.is_empty() {
                bail!("source '{}': `command` is required for kind = \"command\"", src.name);
            }
            let out = run(&src.command, root, timeout)?;
            let body = match src.format {
                SourceFormat::Markdown => out.trim_end().to_string(),
                SourceFormat::Code => format!("```\n{}\n```", out.trim_end()),
            };
            Ok(Section {
                title: src.name.clone(),
                intro: None,
                body,
            })
        }
        "docker" => builtins::docker(src, lang),
        "systemd" => builtins::systemd(src, root, lang, timeout),
        "ports" => builtins::ports(src, lang),
        "projects" => builtins::projects(src, root, lang),
        "scripts" => builtins::scripts(src, root, lang),
        other => bail!(
            "source '{}': unknown kind '{other}' (command, docker, systemd, ports, projects, scripts)",
            src.name
        ),
    }
}

/// Renders one document. `date` is passed in so tests stay deterministic.
pub fn render(src: &SourceSettings, section: Section, lang: Lang, date: &str) -> String {
    let title = src.title.clone().unwrap_or(section.title);
    let mut out = format!(
        "<!-- {} `documentum scan` ({}) — {} -->\n\n# {title}\n\n_{} : {date}_\n",
        lang.t("Generated by", "Généré par"),
        src.name,
        lang.t("do not edit by hand", "ne pas éditer à la main"),
        lang.t("Last scan", "Dernier scan"),
    );
    for intro in [src.intro.as_deref(), section.intro.as_deref()]
        .into_iter()
        .flatten()
    {
        out.push_str(&format!("\n{}\n", intro.trim()));
    }
    out.push_str(&format!("\n{}\n", section.body.trim_end()));
    out
}

/// Runs every declared source (or only `only`), writing changed documents.
/// A failing source is reported and leaves its previous document in place.
pub fn scan(settings: &Settings, root: &Path, only: &[String]) -> Result<Vec<ScanReport>> {
    let lang = Lang::parse(&settings.scan.lang);
    let dir = root.join(&settings.scan.output_dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut reports = Vec::new();
    for src in &settings.scan.source {
        if !only.is_empty() && !only.iter().any(|o| o == &src.name) {
            continue;
        }
        if src.name.is_empty() || src.name.contains(['/', '\\']) || src.name.starts_with('.') {
            bail!("source name '{}' must be a plain file name", src.name);
        }
        let path = dir.join(format!("{}.md", src.name));
        match collect(src, root, lang) {
            Ok(section) => {
                let text = render(src, section, lang, &date);
                let changed = std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str());
                if changed {
                    // Write-then-rename: a concurrent sync never reads half a file.
                    let tmp = path.with_extension("md.tmp");
                    std::fs::write(&tmp, &text)?;
                    std::fs::rename(&tmp, &path)?;
                }
                reports.push(ScanReport {
                    name: src.name.clone(),
                    path,
                    changed,
                    error: None,
                });
            }
            Err(e) => reports.push(ScanReport {
                name: src.name.clone(),
                path,
                changed: false,
                error: Some(format!("{e:#}")),
            }),
        }
    }
    Ok(reports)
}

/// Escapes a value for a markdown table cell.
pub(crate) fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(toml_src: &str) -> SourceSettings {
        let s: Settings = toml::from_str(toml_src).unwrap();
        s.scan.source.into_iter().next().unwrap()
    }

    #[test]
    fn command_source_renders_code_block_and_writes_once() {
        let dir = tempfile::tempdir().unwrap();
        let settings: Settings = toml::from_str(
            r#"
            [scan]
            output_dir = "gen"
            [[scan.source]]
            name = "hello"
            title = "Hello"
            command = ["sh", "-c", "echo hi"]
            "#,
        )
        .unwrap();
        let r = scan(&settings, dir.path(), &[]).unwrap();
        assert!(r[0].error.is_none() && r[0].changed);
        let text = std::fs::read_to_string(dir.path().join("gen/hello.md")).unwrap();
        assert!(text.contains("# Hello") && text.contains("```\nhi\n```"));
        let again = scan(&settings, dir.path(), &[]).unwrap();
        assert!(!again[0].changed);
    }

    #[test]
    fn failing_source_is_reported_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let settings: Settings = toml::from_str(
            "[[scan.source]]\nname = \"bad\"\ncommand = [\"sh\", \"-c\", \"exit 3\"]\n",
        )
        .unwrap();
        let r = scan(&settings, dir.path(), &[]).unwrap();
        assert!(r[0].error.as_deref().unwrap().contains("exited"));
    }

    #[test]
    fn render_is_localised() {
        let src = source("[[scan.source]]\nname = \"x\"\ncommand = [\"true\"]\n");
        let s = Section {
            title: "X".into(),
            intro: Some("intro".into()),
            body: "body".into(),
        };
        let fr = render(&src, s, Lang::Fr, "2026-10-21");
        assert!(fr.contains("_Dernier scan : 2026-10-21_") && fr.contains("\nintro\n"));
    }
}
