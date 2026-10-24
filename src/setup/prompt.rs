//! One-shot picker for `ctxq setup --interactive`. No TUI: one numbered list,
//! one line of input. A missing terminal is the caller's problem — this module
//! only renders and parses.

use std::path::Path;

use super::clients::{
    detect_setup_clients_with, family_installed, CliFamily, CLI_FAMILIES,
};

#[derive(Debug, Clone)]
pub struct MenuRow {
    pub number: usize,
    pub family_id: &'static str,
    pub label: &'static str,
    pub detail: &'static str,
    pub installed: bool,
    pub default_on: bool,
    pub client_ids: &'static [&'static str],
}

#[derive(Debug, Clone)]
pub struct Menu {
    pub rows: Vec<MenuRow>,
    /// Client ids a plain Enter selects. This is the same set `ctxq setup`
    /// would register on its own.
    pub defaults: Vec<&'static str>,
}

pub fn setup_menu(
    workspace: &Path,
    home: Option<&Path>,
    has_bin: impl Fn(&str) -> bool,
) -> Menu {
    let detected = detect_setup_clients_with(workspace, home, &has_bin);
    let rows = CLI_FAMILIES
        .iter()
        .enumerate()
        .map(|(i, family)| row(i + 1, family, home, &has_bin, &detected))
        .collect();
    Menu {
        rows,
        defaults: detected.iter().map(|spec| spec.id).collect(),
    }
}

fn row(
    number: usize,
    family: &CliFamily,
    home: Option<&Path>,
    has_bin: &impl Fn(&str) -> bool,
    detected: &[&super::clients::ClientSpec],
) -> MenuRow {
    MenuRow {
        number,
        family_id: family.id,
        label: family.label,
        detail: family.detail,
        installed: family_installed(family, home, has_bin),
        default_on: family
            .client_ids
            .iter()
            .any(|id| detected.iter().any(|spec| spec.id == *id)),
        client_ids: family.client_ids,
    }
}

pub fn render_menu(menu: &Menu) -> String {
    let mut out = String::from(
        "Which CLIs should get the ctxq MCP server?\n\
Numbers, names, all, or none. Empty line keeps the * defaults.\n\
Copilot, Codex and Grok follow the directory you launch them from.\n\n",
    );
    for row in &menu.rows {
        let mark = if row.default_on { "*" } else { " " };
        let state = if row.installed {
            "installed"
        } else {
            "not installed"
        };
        out.push_str(&format!(
            "  {:>2} {mark} {:<16} {:<42} {state}\n",
            row.number, row.label, row.detail
        ));
    }
    out
}

/// Parses one answer line into client ids.
///
/// Empty keeps `menu.defaults`. `all` selects every listed family. `none`
/// selects nothing. Anything else replaces the defaults: numbers (`1 3`),
/// family ids (`grok`), or client ids (`codex-user`).
pub fn parse_menu_line(line: &str, menu: &Menu) -> Result<Vec<&'static str>, String> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(menu.defaults.clone());
    }
    let tokens: Vec<&str> = line
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|token| !token.is_empty())
        .collect();
    let all = tokens.iter().any(|token| token.eq_ignore_ascii_case("all"));
    let none = tokens.iter().any(|token| token.eq_ignore_ascii_case("none"));
    if all && none {
        return Err("all and none together".into());
    }
    if all {
        return Ok(all_client_ids(menu));
    }
    if none {
        return Ok(Vec::new());
    }
    let mut out: Vec<&str> = Vec::new();
    for token in tokens {
        let row = match token.parse::<usize>() {
            Ok(number) => menu.rows.iter().find(|row| row.number == number),
            Err(_) => menu.rows.iter().find(|row| {
                row.family_id.eq_ignore_ascii_case(token)
                    || row
                        .client_ids
                        .iter()
                        .any(|id| id.eq_ignore_ascii_case(token))
            }),
        };
        let Some(row) = row else {
            return Err(format!(
                "unknown choice {token:?}. Use a number, a name, all, or none"
            ));
        };
        for id in row.client_ids {
            if !out.contains(id) {
                out.push(id);
            }
        }
    }
    Ok(out)
}

fn all_client_ids(menu: &Menu) -> Vec<&'static str> {
    let mut out = Vec::new();
    for row in &menu.rows {
        for id in row.client_ids {
            if !out.contains(id) {
                out.push(id);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu() -> Menu {
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".copilot")).unwrap();
        setup_menu(workspace.path(), Some(home.path()), |bin| bin == "claude")
    }

    #[test]
    fn empty_line_keeps_the_detected_defaults() {
        let menu = menu();
        let ids = parse_menu_line("\n", &menu).unwrap();
        assert!(ids.contains(&"claude"));
        assert!(ids.contains(&"copilot"));
        assert!(!ids.contains(&"vscode"));
        assert!(menu.rows.iter().any(|row| row.family_id == "claude" && row.default_on));
        assert!(menu.rows.iter().any(|row| row.family_id == "vscode" && !row.default_on));
    }

    #[test]
    fn numbers_and_names_replace_the_defaults() {
        let menu = menu();
        let ids = parse_menu_line("1, grok", &menu).unwrap();
        assert_eq!(ids, vec!["claude", "grok", "grok-user"]);
    }

    #[test]
    fn all_and_none() {
        let menu = menu();
        assert!(parse_menu_line("none", &menu).unwrap().is_empty());
        let all = parse_menu_line("all", &menu).unwrap();
        assert!(all.contains(&"copilot"));
        assert!(all.contains(&"windsurf"));
        assert!(parse_menu_line("all none", &menu).is_err());
        assert!(parse_menu_line("nope", &menu).is_err());
    }
}
