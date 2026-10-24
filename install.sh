#!/usr/bin/env sh
# ctxq — one-line install. Binary, checksum, PATH. No root, no compiler.
#
#   curl -fsSL https://ctxq.dev/install.sh | sh && . "$HOME/.ctxq/env"
#
# Then, in a project:  ctxq setup
#
# Downloads a verified archive from GitHub Releases (kerbart/ctxq),
# with https://ctxq.dev/releases/ as fallback. Pin a tag with CTXQ_VERSION=v3.0.1.
# Dest: CTXQ_BINDIR=$HOME/.local/bin
set -eu

BIN="ctxq"
BASE="${CTXQ_BASE_URL:-https://ctxq.dev}"
BINDIR="${CTXQ_BINDIR:-$HOME/.local/bin}"
VERSION="${CTXQ_VERSION:-latest}"
ENVFILE="${CTXQ_ENV:-$HOME/.ctxq/env}"

red()  { printf '\033[38;5;203m%s\033[0m\n' "$*" >&2; }
ok()   { printf '\033[38;5;108m%s\033[0m\n' "$*"; }
dim()  { printf '\033[38;5;245m%s\033[0m\n' "$*"; }
die()  { red "ctxq: $*"; exit 1; }

# Piped into a shell: refuse root so we never write into /root by accident.
if [ "$(id -u)" -eq 0 ]; then
  die "do not run as root. ctxq installs into your home directory (no sudo)."
fi

command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar  >/dev/null 2>&1 || die "tar is required"

printf '\033[38;5;221m▍\033[0m \033[1mctxq\033[0m installer\n'

case "$(uname -s)" in
  Linux)  os="linux" ;;
  Darwin) os="macos" ;;
  MINGW*|MSYS*|CYGWIN*)
    die "native Windows is not shipped. Open WSL and run the same command there."
    ;;
  *) die "unsupported OS: $(uname -s). ctxq ships Linux and macOS binaries." ;;
esac
case "$(uname -m)" in
  x86_64|amd64)  arch="x86_64" ;;
  aarch64|arm64) arch="aarch64" ;;
  *) die "unsupported architecture: $(uname -m)" ;;
esac
ASSET="$BIN-$os-$arch.tar.gz"
dim "target: $os-$arch"

TMP="$(mktemp -d 2>/dev/null || mktemp -d -t ctxq)"
trap 'rm -rf "$TMP"' EXIT

# GitHub Releases are canonical. ctxq.dev mirrors the current build.
# Quiet unless stderr is a TTY — then show a curl progress bar for the archive
# (not for SHA256SUMS, which is a few dozen bytes).
curl_get() {
  name="$1"
  url="$2"
  dest="$3"
  if [ -t 2 ] && [ "$name" != "SHA256SUMS" ]; then
    curl -fL --progress-bar -o "$dest" "$url"
  else
    curl -fsSL -o "$dest" "$url"
  fi
}

REPO="${CTXQ_REPO:-kerbart/ctxq}"

download() {
  # download <name> <dest>
  if [ "$VERSION" = "latest" ]; then
    gh="https://github.com/$REPO/releases/latest/download/$1"
  else
    gh="https://github.com/$REPO/releases/download/$VERSION/$1"
  fi
  if curl_get "$1" "$gh" "$2"; then
    return 0
  fi
  dim "GitHub missed $1, trying $BASE/releases/$1"
  curl_get "$1" "$BASE/releases/$1" "$2" || return 1
}

dim "downloading $ASSET"
if ! download "$ASSET" "$TMP/$ASSET"; then
  die "no binary for $os-$arch (GitHub $REPO + $BASE/releases/$ASSET)
     see https://github.com/$REPO/releases"
fi
if ! download "SHA256SUMS" "$TMP/SHA256SUMS"; then
  die "release has no SHA256SUMS; refusing an unverified binary"
fi

expected="$(awk -v n="$ASSET" '$2 == n || $2 == "*"n { print tolower($1) }' "$TMP/SHA256SUMS" | head -n1)"
[ -n "$expected" ] || die "SHA256SUMS has no entry for $ASSET"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$TMP/$ASSET" | awk '{print tolower($1)}')"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$TMP/$ASSET" | awk '{print tolower($1)}')"
else
  die "need sha256sum or shasum to verify the download"
fi
[ "$actual" = "$expected" ] || die "checksum mismatch for $ASSET"
ok "sha256 verified"

tar -xzf "$TMP/$ASSET" -C "$TMP"
[ -f "$TMP/$BIN" ] || die "archive does not contain $BIN"
[ -x "$TMP/$BIN" ] || chmod +x "$TMP/$BIN"

mkdir -p "$BINDIR"
if [ ! -w "$BINDIR" ]; then
  die "$BINDIR is not writable. Set CTXQ_BINDIR to a directory you own (no sudo)."
fi
mv "$TMP/$BIN" "$BINDIR/$BIN"
chmod +x "$BINDIR/$BIN"

ver="$("$BINDIR/$BIN" --version 2>/dev/null | head -n1 || echo "$VERSION")"
ok "installed $BINDIR/$BIN ($ver)"

# PATH for this user, permanently. The one-liner sources ENVFILE so the
# current shell can run `ctxq` immediately after.
mkdir -p "$(dirname "$ENVFILE")"
printf '# ctxq\nexport PATH="%s:$PATH"\n' "$BINDIR" > "$ENVFILE"

append_rc() {
  # append_rc <file> — create the file if needed so a fresh account still works.
  f="$1"
  marker="# ctxq"
  if ! touch "$f" 2>/dev/null; then
    return 0
  fi
  if grep -q "$marker" "$f" 2>/dev/null; then
    return 0
  fi
  printf '\n%s\n[ -f "%s" ] && . "%s"\n' "$marker" "$ENVFILE" "$ENVFILE" >> "$f"
  dim "PATH wired in $f"
}

append_rc "$HOME/.profile"
case "${SHELL##*/}" in
  zsh)  append_rc "$HOME/.zshrc" ;;
  bash) append_rc "$HOME/.bashrc" ;;
  fish)
    fish_cfg="${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/ctxq.fish"
    mkdir -p "$(dirname "$fish_cfg")"
    printf '# ctxq\nset -gx PATH "%s" $PATH\n' "$BINDIR" > "$fish_cfg"
    dim "PATH wired in $fish_cfg"
    ;;
  *)    append_rc "$HOME/.bashrc"; append_rc "$HOME/.zshrc" ;;
esac

"$BINDIR/$BIN" --help >/dev/null 2>&1 || die "installed binary does not run"

printf '\n'
ok "ctxq is ready."
dim "this shell:    . \"$ENVFILE\""
dim "then, in a repo:"
printf '  ctxq setup\n'
printf '  ctxq search "how do I deploy the api?"\n'
dim "Codex: setup registers MCP when ~/.codex exists. Restart Codex, then /mcp — do not run ctxq in its shell."
