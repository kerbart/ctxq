//! Built-in collectors. Each is optional and only runs when declared.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::{cell, opt_bool, opt_list, opt_str, run, Lang, Section};
use crate::core::settings::SourceSettings;

/// `~/x` → `$HOME/x`; relative paths are resolved against the workspace root.
fn resolve(root: &Path, p: &str) -> PathBuf {
    if p == "~" || p.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(p.trim_start_matches('~').trim_start_matches('/'));
        }
    }
    let path = PathBuf::from(p);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

// ---------------------------------------------------------------- docker

#[derive(Deserialize)]
struct ApiPort {
    #[serde(rename = "IP")]
    ip: Option<String>,
    #[serde(rename = "PrivatePort")]
    private: u16,
    #[serde(rename = "PublicPort")]
    public: Option<u16>,
    #[serde(rename = "Type")]
    proto: String,
}

#[derive(Deserialize)]
struct ApiContainer {
    #[serde(rename = "Names", default)]
    names: Vec<String>,
    #[serde(rename = "Image", default)]
    image: String,
    #[serde(rename = "Ports", default)]
    ports: Vec<ApiPort>,
    #[serde(rename = "Status", default)]
    status: String,
}

/// GET over the Docker Engine unix socket (no docker CLI needed).
fn docker_get(socket: &str, path: &str) -> Result<String> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket)
        .with_context(|| format!("connecting to the Docker socket {socket}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    // HTTP/1.0: the daemon answers without chunked encoding and closes.
    write!(stream, "GET {path} HTTP/1.0\r\nHost: docker\r\n\r\n")?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .context("malformed response from the Docker socket")?;
    let status = head.lines().next().unwrap_or("");
    if !status.contains(" 200 ") {
        bail!("Docker API: {status}");
    }
    Ok(body.to_string())
}

/// Published bindings (`ip:host→container/proto`, IPv4 only — the IPv6 twin
/// adds nothing) or `port/proto (internal)`, and the overall exposure.
fn bindings(ports: &[ApiPort], lang: Lang) -> (Vec<String>, &'static str) {
    let (mut public, mut local) = (false, false);
    let mut seen: Vec<String> = Vec::new();
    // The API lists ports in no stable order: sort (published first) so an
    // unchanged container yields an unchanged document.
    let mut ports: Vec<&ApiPort> = ports.iter().collect();
    ports.sort_by_key(|p| {
        (
            p.public.is_none(),
            p.ip.clone().unwrap_or_default(),
            p.public.unwrap_or(0),
            p.private,
            p.proto.clone(),
        )
    });
    for p in ports {
        let key = match (&p.ip, p.public) {
            (Some(ip), Some(host)) if !ip.contains(':') => {
                if ip == "0.0.0.0" {
                    public = true;
                } else if ip.starts_with("127.") {
                    local = true;
                }
                format!("{ip}:{host}→{}/{}", p.private, p.proto)
            }
            (Some(_), Some(_)) => continue,
            _ => format!(
                "{}/{} ({})",
                p.private,
                p.proto,
                lang.t("internal", "interne")
            ),
        };
        if !seen.contains(&key) {
            seen.push(key);
        }
    }
    let expo = if public {
        lang.t("🌐 all interfaces", "🌐 LAN")
    } else if local {
        "🔒 localhost"
    } else {
        lang.t("internal", "interne")
    };
    (seen, expo)
}

pub fn docker(src: &SourceSettings, lang: Lang) -> Result<Section> {
    let socket = opt_str(src, "socket")
        .map(str::to_string)
        .or_else(|| {
            std::env::var("DOCKER_HOST")
                .ok()
                .and_then(|h| h.strip_prefix("unix://").map(str::to_string))
        })
        .unwrap_or_else(|| "/var/run/docker.sock".into());
    let all = opt_bool(src, "all").unwrap_or(false);
    let body = docker_get(&socket, &format!("/containers/json?all={}", u8::from(all)))?;
    let containers: Vec<ApiContainer> = serde_json::from_str(&body).context("Docker API JSON")?;
    let mut rows: Vec<(String, String, String, &str, String)> = containers
        .iter()
        .map(|c| {
            let name = c
                .names
                .first()
                .map(|n| n.trim_start_matches('/'))
                .unwrap_or("")
                .to_string();
            let (b, expo) = bindings(&c.ports, lang);
            let ports = if b.is_empty() {
                "—".to_string()
            } else {
                b.join("<br>")
            };
            // Untagged images come back as a full digest: shorten like `docker ps`.
            let image = match c.image.strip_prefix("sha256:") {
                Some(d) => d.chars().take(12).collect(),
                None => c.image.clone(),
            };
            (name, image, ports, expo, c.status.clone())
        })
        .collect();
    rows.sort();
    let exposed = rows.iter().filter(|r| r.3.starts_with('🌐')).count();
    let intro = match lang {
        Lang::Fr => format!(
            "{} conteneurs en cours d'exécution ({exposed} exposés sur le LAN en `0.0.0.0`).\n\n\
             Colonne **Ports** : `hostIP:portHost→portConteneur/proto`. **Exposition** : `🌐 LAN` = écoute \
             sur toutes les interfaces, `🔒 localhost` = 127.0.0.1 seulement, `interne` = réseau Docker \
             uniquement (joignable par les autres conteneurs).",
            rows.len()
        ),
        Lang::En => format!(
            "{} running containers ({exposed} published on all interfaces, `0.0.0.0`).\n\n\
             **Ports**: `hostIP:hostPort→containerPort/proto`. **Exposure**: `🌐 all interfaces`, \
             `🔒 localhost` = 127.0.0.1 only, `internal` = Docker network only.",
            rows.len()
        ),
    };
    let mut body = format!(
        "| {} | Image | {} | {} | {} |\n|---|---|---|---|---|\n",
        lang.t("Container", "Conteneur"),
        lang.t("Ports (host→container)", "Ports (host→conteneur)"),
        lang.t("Exposure", "Exposition"),
        lang.t("State", "État"),
    );
    for (name, image, ports, expo, status) in rows {
        body.push_str(&format!(
            "| `{}` | {} | {ports} | {expo} | {} |\n",
            cell(&name),
            cell(&image),
            cell(&status)
        ));
    }
    Ok(Section {
        title: lang.t("Docker inventory", "Inventaire Docker").into(),
        intro: Some(intro),
        body,
    })
}

// ---------------------------------------------------------------- systemd

/// Standard distribution services: noise for a knowledge base.
const SYSTEM_UNITS: &[&str] = &[
    "accounts-daemon",
    "avahi-daemon",
    "bluetooth",
    "colord",
    "containerd",
    "cron",
    "cups",
    "cups-browsed",
    "dbus",
    "docker",
    "fwupd",
    "kerneloops",
    "ModemManager",
    "NetworkManager",
    "nmbd",
    "polkit",
    "power-profiles-daemon",
    "rpc-statd",
    "rpcbind",
    "rsyslog",
    "rtkit-daemon",
    "smbd",
    "snapd",
    "switcheroo-control",
    "thermald",
    "udisks2",
    "unattended-upgrades",
    "upower",
    "wpa_supplicant",
    "wheel",
];
const SYSTEM_PREFIXES: &[&str] = &["systemd-", "user@", "getty@"];

pub fn systemd(
    src: &SourceSettings,
    root: &Path,
    lang: Lang,
    timeout: Duration,
) -> Result<Section> {
    let user = opt_bool(src, "user").unwrap_or(false);
    let exclude = opt_list(src, "exclude")
        .unwrap_or_else(|| SYSTEM_UNITS.iter().map(|s| s.to_string()).collect());
    let prefixes = opt_list(src, "exclude_prefixes")
        .unwrap_or_else(|| SYSTEM_PREFIXES.iter().map(|s| s.to_string()).collect());
    let mut argv: Vec<String> = vec!["systemctl".into()];
    if user {
        argv.push("--user".into());
    }
    argv.extend(
        [
            "list-units",
            "--type=service",
            "--state=running",
            "--no-pager",
            "--no-legend",
            "--plain",
        ]
        .map(String::from),
    );
    let out = run(&argv, root, timeout)?;
    let mut units: Vec<String> = out
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|u| {
            let name = u.trim_end_matches(".service");
            !exclude.iter().any(|e| e == name)
                && !prefixes.iter().any(|p| name.starts_with(p.as_str()))
        })
        .map(str::to_string)
        .collect();
    units.sort();
    let restart = if user {
        "systemctl --user restart"
    } else {
        "sudo systemctl restart"
    };
    let mut body = format!("| Unit | {} |\n|---|---|\n", lang.t("Command", "Commande"));
    for u in &units {
        body.push_str(&format!("| `{u}` | `{restart} {u}` |\n"));
    }
    let intro = match lang {
        Lang::Fr => format!(
            "{} services applicatifs actifs (les services système standards sont filtrés).",
            units.len()
        ),
        Lang::En => format!(
            "{} running application services (standard system services filtered out).",
            units.len()
        ),
    };
    Ok(Section {
        title: if user {
            lang.t(
                "systemd inventory (user services)",
                "Inventaire systemd (services utilisateur)",
            )
            .into()
        } else {
            lang.t(
                "systemd inventory (application services)",
                "Inventaire systemd (services applicatifs)",
            )
            .into()
        },
        intro: Some(intro),
        body,
    })
}

// ---------------------------------------------------------------- ports

/// `/proc/net/tcp*` address (`0100007F:0BB8`) → (`127.0.0.1`, 3000).
fn parse_proc_addr(s: &str) -> Option<(String, u16)> {
    let (ip, port) = s.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let ip = match ip.len() {
        8 => {
            let n = u32::from_str_radix(ip, 16).ok()?;
            std::net::Ipv4Addr::from(n.swap_bytes()).to_string()
        }
        32 => {
            let mut bytes = [0u8; 16];
            for w in 0..4 {
                let word = u32::from_str_radix(&ip[w * 8..w * 8 + 8], 16).ok()?;
                bytes[w * 4..w * 4 + 4].copy_from_slice(&word.to_le_bytes());
            }
            std::net::Ipv6Addr::from(bytes).to_string()
        }
        _ => return None,
    };
    Some((ip, port))
}

/// Socket inode → process name, for the processes we may inspect.
fn socket_owners() -> HashMap<u64, String> {
    let mut map = HashMap::new();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return map;
    };
    for p in procs.flatten() {
        let pid = p.file_name();
        if !pid.to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let comm = std::fs::read_to_string(p.path().join("comm")).unwrap_or_default();
        let Ok(fds) = std::fs::read_dir(p.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if let Ok(target) = std::fs::read_link(fd.path()) {
                let t = target.to_string_lossy();
                if let Some(inode) = t.strip_prefix("socket:[").and_then(|r| r.strip_suffix(']')) {
                    if let Ok(i) = inode.parse() {
                        map.insert(i, comm.trim().to_string());
                    }
                }
            }
        }
    }
    map
}

pub fn ports(src: &SourceSettings, lang: Lang) -> Result<Section> {
    let with_udp = opt_bool(src, "udp").unwrap_or(false);
    let owners = socket_owners();
    // (port, proto) → (addresses, process)
    let mut rows: BTreeMap<(u16, &str), (Vec<String>, String)> = BTreeMap::new();
    let mut tables = vec![
        ("tcp", "/proc/net/tcp", "0A"),
        ("tcp", "/proc/net/tcp6", "0A"),
    ];
    if with_udp {
        tables.push(("udp", "/proc/net/udp", "07"));
        tables.push(("udp", "/proc/net/udp6", "07"));
    }
    for (proto, file, listen_state) in tables {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 10 || f[3] != listen_state {
                continue;
            }
            let Some((ip, port)) = parse_proc_addr(f[1]) else {
                continue;
            };
            let inode: u64 = f[9].parse().unwrap_or(0);
            let e = rows.entry((port, proto)).or_default();
            if !e.0.contains(&ip) {
                e.0.push(ip);
            }
            if e.1.is_empty() {
                if let Some(name) = owners.get(&inode) {
                    e.1 = name.clone();
                }
            }
        }
    }
    let mut body = format!(
        "| Port | Proto | {} | {} |\n|---|---|---|---|\n",
        lang.t("Listening on", "Écoute sur"),
        lang.t("Process", "Processus"),
    );
    for ((port, proto), (addrs, proc_name)) in &rows {
        let proc_name = if proc_name.is_empty() {
            "—"
        } else {
            proc_name.as_str()
        };
        body.push_str(&format!(
            "| {port} | {proto} | {} | {} |\n",
            addrs.join(", "),
            cell(proc_name)
        ));
    }
    Ok(Section {
        title: lang.t("Listening ports", "Ports en écoute").into(),
        intro: Some(match lang {
            Lang::Fr => format!(
                "{} ports en écoute. `—` = processus d'un autre utilisateur (non inspectable sans droits).",
                rows.len()
            ),
            Lang::En => format!(
                "{} listening ports. `—` = process owned by another user (not inspectable without privileges).",
                rows.len()
            ),
        }),
        body,
    })
}

// ---------------------------------------------------------------- projects

/// First "speaking" line of a markdown file (H1 text or first sentence).
fn first_meaningful_line(path: &Path) -> String {
    let text = crate::core::doctext::read_lossy(path).unwrap_or_default();
    for line in crate::core::doctext::split_lines(&text) {
        let s = line.trim().trim_start_matches('#').trim();
        if !s.is_empty() && !["<!--", "![", "|", "---"].iter().any(|p| s.starts_with(p)) {
            return s.chars().take(160).collect();
        }
    }
    String::new()
}

fn sorted_dir(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

pub fn projects(src: &SourceSettings, root: &Path, lang: Lang) -> Result<Section> {
    let dir_opt = opt_str(src, "dir").unwrap_or(".");
    let dir = resolve(root, dir_opt);
    let label = opt_str(src, "label")
        .unwrap_or(dir_opt)
        .trim_end_matches('/')
        .to_string();
    let entries = opt_list(src, "entry_files").unwrap_or_else(|| {
        ["README.md", "AGENTS.md", "CLAUDE.md"]
            .map(String::from)
            .to_vec()
    });
    let exclude = opt_list(src, "exclude").unwrap_or_default();
    let mut rows = Vec::new();
    for top in sorted_dir(&dir) {
        let name = top
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if !top.is_dir() || name.starts_with('.') || exclude.iter().any(|e| e == &name) {
            continue;
        }
        let entry = entries
            .iter()
            .map(|e| top.join(e))
            .find(|p| p.is_file())
            .or_else(|| {
                sorted_dir(&top)
                    .into_iter()
                    .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("md")))
            });
        let desc = entry
            .as_deref()
            .map(first_meaningful_line)
            .unwrap_or_default();
        let entry_label = entry
            .as_deref()
            .and_then(|p| p.file_name())
            .map(|n| format!("`{}`", n.to_string_lossy()))
            .unwrap_or_else(|| "—".into());
        rows.push(format!(
            "| `{label}/{name}` | {entry_label} | {} |",
            cell(&desc)
        ));
    }
    let body = format!(
        "_{} {}._\n\n| {} | {} | Description |\n|---|---|---|\n{}\n",
        rows.len(),
        lang.t("projects", "projets"),
        lang.t("Project", "Projet"),
        lang.t("Entry doc", "Doc d'entrée"),
        rows.join("\n")
    );
    Ok(Section {
        title: format!("{} ({label}/)", lang.t("Project inventory", "Inventaire des projets")),
        intro: Some(
            lang.t(
                "Every project directory and its entry document. Their markdown content is indexed: search rather than grep.",
                "Tous les répertoires-projets et leur doc d'entrée. Le **contenu** de ces markdown est indexé : interroge `ctxq` plutôt que de grep.",
            )
            .into(),
        ),
        body,
    })
}

// ---------------------------------------------------------------- scripts

/// First comment line that reads like a description (not a shebang, not an assignment).
fn script_description(path: &Path) -> String {
    let text = crate::core::doctext::read_lossy(path).unwrap_or_default();
    for line in crate::core::doctext::split_lines(&text).into_iter().take(8) {
        let s = line.trim().trim_start_matches('#').trim();
        let head: String = s.chars().take(12).collect();
        if !s.is_empty() && !s.starts_with('!') && !head.contains('=') {
            return s.to_string();
        }
    }
    String::new()
}

pub fn scripts(src: &SourceSettings, root: &Path, lang: Lang) -> Result<Section> {
    use std::os::unix::fs::PermissionsExt;
    let dirs = opt_list(src, "dirs").unwrap_or_else(|| vec!["bin".into()]);
    let mut body = String::new();
    for d in &dirs {
        let dir = resolve(root, d);
        body.push_str(&format!("\n## {d}\n\n"));
        if !dir.is_dir() {
            body.push_str(lang.t("_(missing directory)_\n", "_(dossier absent)_\n"));
            continue;
        }
        let scripts: Vec<PathBuf> = sorted_dir(&dir)
            .into_iter()
            .filter(|p| {
                p.is_file()
                    && std::fs::metadata(p)
                        .map(|m| m.permissions().mode() & 0o111 != 0)
                        .unwrap_or(false)
            })
            .collect();
        if scripts.is_empty() {
            body.push_str(lang.t(
                "_(no executable script)_\n",
                "_(aucun script exécutable)_\n",
            ));
        }
        for p in scripts {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            body.push_str(&format!("- `{d}/{name}` — {}\n", script_description(&p)));
        }
    }
    Ok(Section {
        title: lang.t("Scripts", "Scripts maison").into(),
        intro: None,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_proc_net_addresses() {
        assert_eq!(
            parse_proc_addr("0100007F:0BB8"),
            Some(("127.0.0.1".into(), 3000))
        );
        assert_eq!(
            parse_proc_addr("00000000:0050"),
            Some(("0.0.0.0".into(), 80))
        );
        assert_eq!(
            parse_proc_addr("00000000000000000000000001000000:1F90"),
            Some(("::1".into(), 8080))
        );
    }

    #[test]
    fn docker_bindings_keep_ipv4_and_classify_exposure() {
        let ports: Vec<ApiPort> = serde_json::from_str(
            r#"[{"IP":"0.0.0.0","PrivatePort":80,"PublicPort":3011,"Type":"tcp"},
                {"IP":"::","PrivatePort":80,"PublicPort":3011,"Type":"tcp"},
                {"PrivatePort":5432,"Type":"tcp"}]"#,
        )
        .unwrap();
        let (b, expo) = bindings(&ports, Lang::Fr);
        assert_eq!(b, ["0.0.0.0:3011→80/tcp", "5432/tcp (interne)"]);
        let reversed: Vec<ApiPort> = serde_json::from_str(
            r#"[{"PrivatePort":5432,"Type":"tcp"},{"IP":"0.0.0.0","PrivatePort":80,"PublicPort":3011,"Type":"tcp"}]"#,
        )
        .unwrap();
        assert_eq!(
            bindings(&reversed, Lang::Fr).0,
            b,
            "order must not depend on the API"
        );
        assert_eq!(expo, "🌐 LAN");
    }

    #[test]
    fn projects_and_scripts_describe_directories() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("alpha")).unwrap();
        std::fs::write(root.join("alpha/README.md"), "# Alpha — does things\n").unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin/go.sh"), "#!/bin/sh\n# Launches the thing\n").unwrap();
        std::fs::set_permissions(
            root.join("bin/go.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let src: SourceSettings = toml::from_str::<crate::core::settings::Settings>(
            "[[scan.source]]\nname = \"p\"\nkind = \"projects\"\nexclude = [\"bin\"]\n",
        )
        .unwrap()
        .scan
        .source
        .remove(0);
        let s = projects(&src, root, Lang::En).unwrap();
        assert!(
            s.body
                .contains("| `./alpha` | `README.md` | Alpha — does things |"),
            "{}",
            s.body
        );
        let src: SourceSettings = toml::from_str::<crate::core::settings::Settings>(
            "[[scan.source]]\nname = \"s\"\nkind = \"scripts\"\n",
        )
        .unwrap()
        .scan
        .source
        .remove(0);
        let s = scripts(&src, root, Lang::En).unwrap();
        assert!(
            s.body.contains("- `bin/go.sh` — Launches the thing"),
            "{}",
            s.body
        );
    }
}
