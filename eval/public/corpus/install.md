# Install

ctxq ships as one binary. The install line is:

```
curl -fsSL https://ctxq.dev/install.sh | sh && . "$HOME/.ctxq/env"
```

It downloads a SHA-256-verified archive from GitHub Releases
(`kerbart/ctxq`), with `https://ctxq.dev/releases/` as a mirror,
installs into `~/.local/bin`, and writes `~/.ctxq/env` so PATH is set in this
shell. No root, no compiler, no cargo fallback.

Then, from a project directory:

```
ctxq setup
```

`ctxq setup` from `$HOME` is refused unless you pass `--force-home`.
Each workspace root has its own index under `~/.cache/ctxq/<name>-<hash>/`.
Indexing `$HOME` and then `~/engie` makes two indexes; they do not merge.
If Codex is installed (`~/.codex/`), setup registers the MCP server so the
agent does not need to run `ctxq` in its sandboxed shell.
Windows uses WSL. Linux arm64 and macOS archives ship with tagged GitHub releases.
