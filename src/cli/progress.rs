//! Terminal progress for `index` / `sync` / `setup`.
//!
//! TTY: spinner while walking, then a determinate bar with rate and ETA.
//! Piped / `CTXQ_NO_PROGRESS`: throttled one-line status, no control codes.

use std::io::IsTerminal;
use std::time::{Duration, Instant};

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle};

use crate::cli::config::ProjectPaths;
use crate::core::embed_hint::Upgrade;
use crate::core::hybrid::{IndexHooks, SyncReport};
use crate::core::vector::Embedder;

const GOLD: &str = "\x1b[38;5;221m";
const DIM: &str = "\x1b[38;5;245m";
const GREEN: &str = "\x1b[38;5;108m";
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";

pub struct IndexProgress {
    color: bool,
    bar: bool,
    started: Instant,
    last_plain: Instant,
    pb: ProgressBar,
    phase: Phase,
    changed: usize,
    verb: &'static str,
    paths: ProjectPaths,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Scan,
    Files,
    Code,
}

fn want_bar() -> bool {
    std::io::stderr().is_terminal()
        && std::env::var_os("CTXQ_NO_PROGRESS").is_none()
        && std::env::var("TERM").ok().as_deref() != Some("dumb")
}

fn want_color() -> bool {
    std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn rate_key(state: &ProgressState, w: &mut dyn std::fmt::Write) {
    let e = state.elapsed().as_secs_f64().max(0.001);
    let r = state.pos() as f64 / e;
    let _ = write!(w, "{r:.0}/s");
}

fn scan_style() -> ProgressStyle {
    ProgressStyle::with_template("  {spinner:.yellow} scanning  {wide_msg}")
        .unwrap()
        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ")
}

fn files_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "  {bar:22.yellow/black} {pos:>4}/{len:<4} {percent:>3}%  {rate}  eta {eta}  {wide_msg}",
    )
    .unwrap()
    .with_key("rate", rate_key)
    .progress_chars("━╸─")
}

fn code_style() -> ProgressStyle {
    ProgressStyle::with_template("  {spinner:.yellow} code graph  {wide_msg}")
        .unwrap()
        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ")
}

impl IndexProgress {
    pub fn start(paths: &ProjectPaths, verb: &'static str) -> Self {
        let color = want_color();
        let bar = want_bar();
        let mark = if color {
            format!("{GOLD}▍{RESET}")
        } else {
            "▍".into()
        };
        let verb_s = if color {
            format!("{BOLD}{verb}{RESET}")
        } else {
            verb.to_string()
        };
        let path = paths.root_dir.display().to_string();
        let path_s = if color {
            format!("{DIM}{path}{RESET}")
        } else {
            path
        };
        eprintln!("{mark} {verb_s}  {path_s}");

        let pb = if bar {
            let pb = ProgressBar::new_spinner();
            pb.set_draw_target(ProgressDrawTarget::stderr());
            pb.set_style(scan_style());
            pb.enable_steady_tick(Duration::from_millis(80));
            pb
        } else {
            ProgressBar::hidden()
        };

        Self {
            color,
            bar,
            started: Instant::now(),
            last_plain: Instant::now() - Duration::from_secs(1),
            pb,
            phase: Phase::Scan,
            changed: 0,
            verb,
            paths: paths.clone(),
        }
    }

    fn paint_ok(&self, s: &str) -> String {
        if self.color {
            format!("{GREEN}{s}{RESET}")
        } else {
            s.to_string()
        }
    }

    fn paint_dim(&self, s: &str) -> String {
        if self.color {
            format!("{DIM}{s}{RESET}")
        } else {
            s.to_string()
        }
    }

    fn paint_hint(&self, s: &str) -> String {
        if self.color {
            format!("{GOLD}{s}{RESET}")
        } else {
            s.to_string()
        }
    }

    fn plain(&mut self, line: String) {
        if self.bar {
            return;
        }
        if self.last_plain.elapsed() < Duration::from_millis(400) {
            return;
        }
        self.last_plain = Instant::now();
        eprintln!("  {line}");
    }

    pub fn begin_code(&mut self) {
        if self.phase == Phase::Code {
            return;
        }
        self.phase = Phase::Code;
        if self.bar {
            self.pb.disable_steady_tick();
            self.pb.set_position(0);
            self.pb.set_length(0);
            self.pb.set_style(code_style());
            self.pb.enable_steady_tick(Duration::from_millis(80));
            self.pb.set_message("…");
        }
    }

    pub fn finish(
        &mut self,
        report: &SyncReport,
        code: Option<&crate::code::sync::SyncReport>,
        embedder: &Embedder,
    ) {
        self.pb.finish_and_clear();
        let elapsed = fmt_dur(self.started.elapsed());
        let check = self.paint_ok("✓");
        eprintln!(
            "{check}  {} files  {} chunks  {elapsed}",
            report.files, report.chunks
        );
        let mut extra = Vec::new();
        if report.changed > 0 && report.changed != report.files {
            extra.push(format!("{} updated", report.changed));
        }
        if report.empty > 0 {
            extra.push(format!("{} empty", report.empty));
        }
        if report.quarantined > 0 {
            extra.push(format!("{} quarantined", report.quarantined));
        }
        if report.unusable > 0 {
            extra.push(format!("{} unusable", report.unusable));
        }
        if let Some(c) = code {
            extra.push(format!(
                "code {} extracted · {} unchanged",
                c.extracted, c.skipped_unchanged
            ));
        }
        if !extra.is_empty() {
            eprintln!("   {}", extra.join("  ·  "));
        }
        for line in crate::cli::config::describe_index(&self.paths).lines() {
            eprintln!("{}", self.paint_dim(line));
        }
        if embedder.is_local_hash() {
            if let Some(upgrade) = Upgrade::assess(report.files, report.chunks, true) {
                for line in upgrade.render_block().lines() {
                    eprintln!("{}", self.paint_hint(line));
                }
            } else {
                eprintln!(
                    "   {}",
                    self.paint_dim(
                        "embedder  local-hash  ·  BM25 leads  ·  ollama/openai for neural"
                    )
                );
            }
        } else {
            eprintln!("   embedder  {}", embedder.id());
        }
        let _ = self.verb;
    }
}

impl IndexHooks for IndexProgress {
    fn on_scan(&mut self, found: usize, rel: &str) {
        let msg = ellipsize(rel, 48);
        if self.bar {
            self.pb.set_position(found as u64);
            self.pb.set_message(format!("{found} files   {msg}"));
        } else {
            self.plain(format!("scanning  {found} files"));
        }
    }

    fn on_plan(&mut self, total: usize) {
        self.phase = Phase::Files;
        if self.bar {
            self.pb.disable_steady_tick();
            if total == 0 {
                self.pb.finish_and_clear();
                return;
            }
            self.pb.set_position(0);
            self.pb.set_length(total as u64);
            self.pb.set_style(files_style());
            self.pb.set_message("…");
        } else {
            eprintln!("  {total} files to index");
            self.last_plain = Instant::now() - Duration::from_secs(1);
        }
    }

    fn on_file(&mut self, i: usize, total: usize, rel: &str, changed: bool) {
        if changed {
            self.changed += 1;
        }
        let n = (i + 1) as u64;
        if self.bar {
            self.pb.set_position(n);
            self.pb.set_message(ellipsize(rel, 40));
            return;
        }
        if total == 0 {
            return;
        }
        let elapsed = self.started.elapsed().as_secs_f64().max(0.001);
        let rate = n as f64 / elapsed;
        let left = (total as u64).saturating_sub(n);
        let eta = if rate > 0.05 {
            fmt_dur(Duration::from_secs_f64(left as f64 / rate))
        } else {
            "…".into()
        };
        self.plain(format!(
            "{n}/{total}  {rate:.0}/s  eta {eta}  {}",
            ellipsize(rel, 32)
        ));
    }

    fn on_code(&mut self, scanned: usize, rel: &str) {
        if self.phase != Phase::Code {
            self.begin_code();
        }
        if self.bar {
            self.pb.set_position(scanned as u64);
            self.pb
                .set_message(format!("{scanned} files   {}", ellipsize(rel, 40)));
        } else {
            self.plain(format!("code graph  {scanned} files"));
        }
    }
}

pub fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

pub fn ellipsize(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let take = max.saturating_sub(1);
    let tail: String = s
        .chars()
        .rev()
        .take(take)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::{ellipsize, fmt_dur};
    use std::time::Duration;

    #[test]
    fn ellipsize_keeps_short_paths() {
        assert_eq!(ellipsize("src/lib.rs", 40), "src/lib.rs");
    }

    #[test]
    fn ellipsize_keeps_the_tail() {
        let s = ellipsize("very/long/prefix/docs/api.md", 12);
        assert!(s.starts_with('…'), "{s}");
        assert!(s.ends_with("api.md"), "{s}");
        assert_eq!(s.chars().count(), 12);
    }

    #[test]
    fn fmt_dur_buckets() {
        assert_eq!(fmt_dur(Duration::from_secs(9)), "9s");
        assert_eq!(fmt_dur(Duration::from_secs(75)), "1m15s");
        assert_eq!(fmt_dur(Duration::from_secs(3661)), "1h01m");
    }
}
