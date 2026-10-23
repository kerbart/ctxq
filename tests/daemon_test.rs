//! P4 gate (local embedder, no network): `q` starts the daemon on demand,
//! answers through it, the HTTP API serves peers, `--stop` stops it.

use std::process::Command;
use std::time::{Duration, Instant};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_documentum"))
}

#[test]
fn q_autostarts_daemon_and_http_serves_peers() {
    let ws = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let root = ws.path();
    std::fs::write(
        root.join("svc.md"),
        "# Services\n\n## Alpha\n\nLe service alpha écoute sur le port 4100.\n",
    )
    .unwrap();
    std::fs::write(
        root.join("documentum.toml"),
        "[daemon]\nhttp = \"127.0.0.1:39517\"\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let out = bin()
            .arg("--path")
            .arg(root)
            .args(args)
            .env("DOCUMENTUM_CACHE_DIR", cache.path())
            .env_remove("DOCUMENTUM_CONFIG")
            .env_remove("DOCUMENTUM_EMBEDDINGS")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    run(&["sync"]);
    let text = run(&["search", "port du service alpha", "-l", "1"]);
    assert!(
        text.contains("[1] score") && text.contains("→ svc.md:3-5"),
        "{text}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&run(&["search", "alpha", "--json", "-l", "1"])).unwrap();
    assert_eq!(json["results"][0]["source"], "svc.md");
    // `q` stays as a hidden alias with the historical list shape.
    let legacy: serde_json::Value =
        serde_json::from_str(&run(&["q", "alpha", "--json", "--k", "1"])).unwrap();
    assert_eq!(legacy[0]["source"], "svc.md");

    // HTTP API (what a federated peer calls).
    let deadline = Instant::now() + Duration::from_secs(10);
    let body = r#"{"query":"port alpha","limit":1}"#;
    let resp = loop {
        if let Ok(mut s) = std::net::TcpStream::connect("127.0.0.1:39517") {
            use std::io::{Read, Write};
            write!(
                s,
                "POST /search HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            break out;
        }
        assert!(Instant::now() < deadline, "HTTP API never came up");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        resp.starts_with("HTTP/1.1 200") && resp.contains("svc.md"),
        "{resp}"
    );

    // A second workspace federating the first one as a peer.
    let ws2 = tempfile::tempdir().unwrap();
    std::fs::write(
        ws2.path().join("own.md"),
        "# Own\n\n## Beta\n\nLa base beta tourne sous PostgreSQL.\n",
    )
    .unwrap();
    std::fs::write(
        ws2.path().join("documentum.toml"),
        "[[store.federate]]\nbackend = \"peer\"\nurl = \"http://127.0.0.1:39517\"\nname = \"ws1\"\nweight = 5.0\n",
    )
    .unwrap();
    let run2 = |args: &[&str]| {
        let out = bin()
            .arg("--path")
            .arg(ws2.path())
            .args(args)
            .env("DOCUMENTUM_CACHE_DIR", cache.path())
            .env_remove("DOCUMENTUM_CONFIG")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    run2(&["sync"]);
    let fed: serde_json::Value = serde_json::from_str(&run2(&[
        "search",
        "port du service alpha",
        "--json",
        "--no-daemon",
        "-l",
        "3",
    ]))
    .unwrap();
    let hits = fed["results"].as_array().unwrap();
    assert!(
        hits.iter()
            .any(|h| h["origin"] == "ws1" && h["source"] == "svc.md"),
        "{fed}"
    );

    let status: serde_json::Value = serde_json::from_str(&run(&["status"])).unwrap();
    assert_eq!(status["files"], 1);
    assert!(run(&["daemon", "--stop"]).contains("daemon stopped"));
}
