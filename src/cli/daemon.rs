//! Resident daemon and its `q` client.
//!
//! Loading the index (chunks, vectors, inverted index) costs about a second;
//! a query on a warm index costs milliseconds. The daemon keeps one engine warm
//! behind a unix socket (and, optionally, a small HTTP API for federated
//! peers); `documentum q` talks to it, starts it when absent, and falls back to
//! an in-process search when it cannot.
//!
//! Protocol (socket): one JSON object per line, `{"op": "search", ...}` →
//! one JSON line back (`{"ok": true, "data": …}` or `{"ok": false, "error": …}`).

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::cli::config::ProjectPaths;
use crate::core::agent;
use crate::core::hybrid::DocEngine;
use crate::core::settings::Settings;
use crate::core::vector::Embedder;

pub fn socket_path(paths: &ProjectPaths, settings: &Settings) -> PathBuf {
    settings
        .daemon
        .socket
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| paths.cache_dir.join("daemon.sock"))
}

type Job = (Value, mpsc::Sender<Value>);

/// Executes one request against the warm engine.
fn handle(engine: &DocEngine, root: &Path, req: &Value) -> Result<Value> {
    let s = |k: &str| req.get(k).and_then(|v| v.as_str());
    let n = |k: &str, d: u64| req.get(k).and_then(|v| v.as_u64()).unwrap_or(d) as usize;
    match s("op").unwrap_or("search") {
        "search" => agent::search(
            engine,
            root,
            &serde_json::from_value(req.clone()).context("search arguments")?,
        ),
        "status" => agent::status(engine, root),
        "inventory" => agent::inventory(engine, root, s("kind"), s("filter"), n("limit", 50)),
        "map" => agent::map(engine, root, s("prefix"), n("depth", 2), n("limit", 200)),
        "annotate" => {
            let tags: Vec<String> = req
                .get("tags")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            let path = s("path").context("annotate: 'path' is required")?;
            agent::annotate(
                engine,
                path,
                s("summary"),
                &tags,
                s("author").unwrap_or("agent"),
            )
        }
        "ping" => Ok(json!({ "pong": true, "pid": std::process::id() })),
        other => bail!("unknown op '{other}' (search, status, inventory, map, annotate, ping)"),
    }
}

/// The engine lives on one thread (SQLite connection, caches); everything
/// else talks to it through a channel.
fn worker(paths: ProjectPaths, embedder: Embedder, rx: mpsc::Receiver<Job>) -> Result<()> {
    let engine = DocEngine::open_with_embedder(&paths.docs_db, embedder)?;
    let root = paths.root_dir.clone();
    let last_sync = |e: &DocEngine| -> Option<String> {
        e.conn()
            .query_row(
                "SELECT value FROM index_meta WHERE key = 'last_sync'",
                [],
                |r| r.get(0),
            )
            .ok()
    };
    // Warm-up: the first real query must not pay the load.
    let _ = engine.search("warmup", 1);
    let mut seen_sync = last_sync(&engine);
    for (req, reply) in rx {
        // Another process (CLI sync, refresher) rewrote the index: reload.
        let now = last_sync(&engine);
        if now != seen_sync {
            engine.invalidate();
            seen_sync = now;
        }
        let out = match handle(&engine, &root, &req) {
            Ok(data) => json!({ "ok": true, "data": data }),
            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
        };
        let _ = reply.send(out);
    }
    Ok(())
}

fn serve_socket(listener: UnixListener, tx: mpsc::Sender<Job>) {
    for stream in listener.incoming().flatten() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(match stream.try_clone() {
                Ok(s) => s,
                Err(_) => return,
            });
            let mut writer = stream;
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                let req: Value = serde_json::from_str(line.trim())
                    .unwrap_or_else(|e| json!({ "op": "invalid", "error": e.to_string() }));
                let (rtx, rrx) = mpsc::channel();
                if tx.send((req, rtx)).is_err() {
                    return;
                }
                let resp = rrx
                    .recv()
                    .unwrap_or_else(|_| json!({ "ok": false, "error": "daemon stopping" }));
                if writeln!(writer, "{resp}").is_err() {
                    return;
                }
                line.clear();
            }
        });
    }
}

/// Minimal HTTP/1.1 API for peers and tools: `POST /search|/inventory|/map|/annotate`
/// (JSON body), `GET /status`, `GET /health`. Bearer token when configured.
fn serve_http(addr: String, token: Option<String>, tx: mpsc::Sender<Job>) {
    let listener = match std::net::TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("HTTP API: cannot bind {addr}: {e}");
            return;
        }
    };
    tracing::info!("HTTP API on http://{addr}");
    for stream in listener.incoming().flatten() {
        let tx = tx.clone();
        let token = token.clone();
        std::thread::spawn(move || {
            let _ = http_one(stream, token.as_deref(), &tx);
        });
    }
}

fn http_one(
    mut stream: std::net::TcpStream,
    token: Option<&str>,
    tx: &mpsc::Sender<Job>,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let (mut length, mut auth) = (0usize, None::<String>);
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        let (k, v) = h.split_once(':').unwrap_or((&h, ""));
        match k.trim().to_ascii_lowercase().as_str() {
            "content-length" => length = v.trim().parse().unwrap_or(0).min(1 << 20),
            "authorization" => auth = Some(v.trim().to_string()),
            _ => {}
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    let respond = |stream: &mut std::net::TcpStream, code: u16, v: &Value| -> Result<()> {
        let text = v.to_string();
        let reason = match code {
            200 => "OK",
            401 => "Unauthorized",
            404 => "Not Found",
            _ => "Bad Request",
        };
        write!(
            stream,
            "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        )?;
        Ok(())
    };
    if let Some(t) = token {
        if auth.as_deref() != Some(&format!("Bearer {t}")) {
            return respond(
                &mut stream,
                401,
                &json!({ "error": "missing or wrong bearer token" }),
            );
        }
    }
    let op = match (method, path.split('?').next().unwrap_or("")) {
        ("GET", "/health") => return respond(&mut stream, 200, &json!({ "ok": true })),
        ("GET", "/status") => "status",
        ("POST", "/search") => "search",
        ("POST", "/inventory") => "inventory",
        ("POST", "/map") => "map",
        ("POST", "/annotate") => "annotate",
        _ => return respond(&mut stream, 404, &json!({ "error": "unknown route" })),
    };
    let mut req: Value = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&body).unwrap_or(json!({}))
    };
    req["op"] = json!(op);
    let (rtx, rrx) = mpsc::channel();
    tx.send((req, rtx)).context("daemon stopping")?;
    let resp = rrx.recv().context("daemon stopping")?;
    if resp["ok"].as_bool() == Some(true) {
        respond(&mut stream, 200, &resp["data"])
    } else {
        respond(&mut stream, 400, &json!({ "error": resp["error"] }))
    }
}

fn sync_code(paths: &ProjectPaths, settings: &Settings) {
    if let Err(e) = crate::cli::commands::sync_code_graph(paths, settings) {
        tracing::warn!("code sync failed: {e:#}");
    }
}

/// Periodic re-sync / re-scan in a separate engine, so queries keep being
/// answered meanwhile; the worker reloads when `last_sync` moves.
/// The code graph shares `docs.db` and is refreshed on the same cadence.
fn refresher(paths: ProjectPaths, settings: Settings, embedder: Embedder) {
    sync_code(&paths, &settings);
    let sync_every = settings.daemon.sync_interval_secs;
    let scan_every = settings.daemon.scan_interval_secs;
    if sync_every == 0 && scan_every == 0 {
        return;
    }
    let (mut last_sync, mut last_scan) = (Instant::now(), Instant::now());
    loop {
        std::thread::sleep(Duration::from_secs(15));
        let scan_due = scan_every > 0 && last_scan.elapsed().as_secs() >= scan_every;
        let sync_due = scan_due || (sync_every > 0 && last_sync.elapsed().as_secs() >= sync_every);
        if scan_due {
            last_scan = Instant::now();
            if let Err(e) = crate::core::sources::scan(&settings, &paths.root_dir, &[]) {
                tracing::warn!("scheduled scan failed: {e:#}");
            }
        }
        if sync_due {
            last_sync = Instant::now();
            let res = DocEngine::open_with_embedder(&paths.docs_db, embedder.clone()).and_then(
                |mut e| {
                    e.sync_directory_incremental(
                        &paths.root_dir,
                        None::<fn(usize, bool)>,
                        None::<fn(usize, usize)>,
                        None::<fn(usize, usize)>,
                    )
                },
            );
            if let Err(e) = res {
                tracing::warn!("scheduled sync failed: {e:#}");
            }
            sync_code(&paths, &settings);
        }
    }
}

pub fn run_daemon(target: Option<&Path>, embedder: Embedder, http: Option<String>) -> Result<()> {
    let paths = ProjectPaths::discover(target)?;
    if !paths.docs_db.exists() {
        bail!(
            "no index for {}: run `documentum sync` first",
            paths.root_dir.display()
        );
    }
    let settings = Settings::load(&paths.root_dir)?;
    let sock = socket_path(&paths, &settings);
    if UnixStream::connect(&sock).is_ok() {
        bail!("a daemon already answers on {}", sock.display());
    }
    let _ = std::fs::remove_file(&sock);
    if let Some(dir) = sock.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener =
        UnixListener::bind(&sock).with_context(|| format!("binding {}", sock.display()))?;
    // Only the owner may talk to the index.
    std::fs::set_permissions(&sock, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;

    let (tx, rx) = mpsc::channel::<Job>();
    let worker_paths = paths.clone();
    let worker_embedder = embedder.clone();
    let worker = std::thread::spawn(move || worker(worker_paths, worker_embedder, rx));
    let http = http.or_else(|| settings.daemon.http.clone());
    if let Some(addr) = http {
        let token = settings
            .daemon
            .http_token_env
            .as_deref()
            .and_then(|k| std::env::var(k).ok());
        let tx = tx.clone();
        std::thread::spawn(move || serve_http(addr, token, tx));
    }
    {
        let (p, s, e) = (paths.clone(), settings.clone(), embedder);
        std::thread::spawn(move || refresher(p, s, e));
    }
    eprintln!(
        "documentum daemon: {} (pid {})",
        sock.display(),
        std::process::id()
    );
    std::thread::spawn(move || serve_socket(listener, tx));
    let res = worker
        .join()
        .map_err(|_| anyhow::anyhow!("worker thread panicked"))?;
    let _ = std::fs::remove_file(&sock);
    res
}

/// Sends one request to the daemon, starting it if needed. `None` when no
/// daemon could be reached (the caller then answers in-process).
pub fn ask(
    paths: &ProjectPaths,
    settings: &Settings,
    req: &Value,
    autostart: bool,
) -> Option<Value> {
    let sock = socket_path(paths, settings);
    let mut stream = UnixStream::connect(&sock).ok().or_else(|| {
        if !autostart || !spawn_daemon(paths).is_ok() {
            return None;
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if let Ok(s) = UnixStream::connect(&sock) {
                return Some(s);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .ok()?;
    writeln!(stream, "{req}").ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

fn spawn_daemon(paths: &ProjectPaths) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()?;
    std::fs::create_dir_all(&paths.cache_dir)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.cache_dir.join("daemon.log"))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--path")
        .arg(&paths.root_dir)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log);
    // Detached session: survives the shell (and agent run) that started it.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()?;
    Ok(())
}

/// Stops the daemon of this workspace (it restarts on the next `q`).
pub fn stop_daemon(paths: &ProjectPaths, settings: &Settings) -> Result<bool> {
    let Some(resp) = ask(paths, settings, &json!({ "op": "ping" }), false) else {
        return Ok(false);
    };
    let pid = resp["data"]["pid"]
        .as_i64()
        .context("daemon did not report its pid")?;
    // SAFETY: plain signal to a pid the daemon reported for itself.
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    let _ = std::fs::remove_file(socket_path(paths, settings));
    Ok(true)
}

// ------------------------------------------------------------------ output

/// First meaningful lines of a passage, heading skipped (documentum format).
fn snippet(text: &str, max_lines: usize) -> String {
    let mut lines: Vec<&str> = crate::core::doctext::split_lines(text);
    if lines
        .first()
        .is_some_and(|l| l.trim_start().starts_with('#'))
    {
        lines.remove(0);
    }
    lines
        .into_iter()
        .filter(|l| !l.trim().is_empty())
        .take(max_lines)
        .map(|l| shorten(l.trim(), 160))
        .collect::<Vec<_>>()
        .join("\n    ")
}

/// Whitespace-collapsing word-boundary truncation (Python's `textwrap.shorten`).
fn shorten(text: &str, width: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= width {
        return collapsed;
    }
    const PLACEHOLDER: &str = " …";
    let budget = width.saturating_sub(PLACEHOLDER.chars().count());
    let mut kept = String::new();
    for word in collapsed.split(' ') {
        let extra = usize::from(!kept.is_empty());
        if kept.chars().count() + extra + word.chars().count() > budget {
            break;
        }
        if extra == 1 {
            kept.push(' ');
        }
        kept.push_str(word);
    }
    format!("{kept}{PLACEHOLDER}")
}

/// Human rendering, kept stable for scripts.
pub fn format_results(query: &str, data: &Value, full: bool, lang_fr: bool) -> String {
    let header = if lang_fr {
        "Résultats pour"
    } else {
        "Results for"
    };
    let mut out = vec![format!("## {header} : \"{query}\"\n")];
    for r in data["results"].as_array().into_iter().flatten() {
        let score = r["score"].as_f64().unwrap_or(0.0);
        let crumb = r["breadcrumb"]
            .as_str()
            .or(r["title"].as_str())
            .unwrap_or("");
        let origin = r["origin"]
            .as_str()
            .map(|o| format!(" [{o}]"))
            .unwrap_or_default();
        out.push(format!(
            "[{}] score {} — {crumb}{origin}",
            r["rank"],
            (score * 1000.0).round() / 1000.0
        ));
        let text = r["text"].as_str().unwrap_or("");
        if full {
            out.push(
                crate::core::doctext::split_lines(text)
                    .into_iter()
                    .map(|l| format!("    {l}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        } else {
            out.push(format!("    {}", snippet(text, 6)));
        }
        let lines = &r["lines"];
        out.push(format!(
            "    → {}:{}-{}\n",
            r["source"].as_str().unwrap_or(""),
            lines[0].as_u64().unwrap_or(0),
            lines[1].as_u64().unwrap_or(0)
        ));
    }
    out.join("\n")
}

/// legacy JSON shape (a bare list), for existing scripts.
pub fn legacy_json(data: &Value) -> Value {
    Value::Array(
        data["results"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| {
                json!({
                    "rank": r["rank"],
                    "score": r["score"],
                    "source": r["source"],
                    "title": r["title"],
                    "breadcrumb": r["breadcrumb"],
                    "line_start": r["lines"][0],
                    "line_end": r["lines"][1],
                    "text": r["text"],
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_legacy_text() {
        let data = json!({ "results": [{
            "rank": 1, "score": 0.9673, "source": "~/myapp/CLAUDE.md", "lines": [12, 52],
            "breadcrumb": "myapp > Ce qu'est myapp",
            "text": "## Ce qu'est myapp\n\nApp   single-user.\n- port **3917**",
        }]});
        let out = format_results("port de myapp", &data, false, true);
        assert_eq!(
            out,
            "## Résultats pour : \"port de myapp\"\n\n[1] score 0.967 — myapp > Ce qu'est myapp\n    App single-user.\n    - port **3917**\n    → ~/myapp/CLAUDE.md:12-52\n"
        );
        assert_eq!(legacy_json(&data)[0]["line_end"], 52);
    }
}
