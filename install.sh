#!/usr/bin/env sh
# documentum installer — one binary, no runtime dependency.
#   curl -fsSL https://documentum.dev/install.sh | bash
#
# Downloads the latest GitHub release for this OS/arch, verifies it against
# the release's SHA256SUMS, installs to ~/.local/bin (no root).
#
#   DOCUMENTUM_VERSION=v3.0.1   pin a release tag (default: latest)
#   DOCUMENTUM_BINDIR=/usr/local/bin   install elsewhere
#   GH_TOKEN / GITHUB_TOKEN     only for private forks, or to lift API rate limits
set -eu

REPO="${DOCUMENTUM_REPO:-kerbart/documentum}"
BIN="documentum"
BINDIR="${DOCUMENTUM_BINDIR:-$HOME/.local/bin}"
VERSION="${DOCUMENTUM_VERSION:-latest}"
TOKEN="${GH_TOKEN:-${GITHUB_TOKEN:-}}"

red()  { printf '\033[38;5;203m%s\033[0m\n' "$*" >&2; }
ok()   { printf '\033[38;5;108m%s\033[0m\n' "$*"; }
dim()  { printf '\033[2m%s\033[0m\n' "$*"; }
die()  { red "documentum: $*"; exit 1; }

command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar  >/dev/null 2>&1 || die "tar is required"

printf '\033[38;5;221m▍\033[0m \033[1mdocumentum\033[0m installer\n'

# --- target -----------------------------------------------------------------
case "$(uname -s)" in
  Linux)  os="linux" ;;
  Darwin) os="macos" ;;
  MINGW*|MSYS*|CYGWIN*) die "native Windows build: coming soon. Run this installer inside WSL meanwhile." ;;
  *) die "unsupported OS: $(uname -s)" ;;
esac
case "$(uname -m)" in
  x86_64|amd64)  arch="x86_64" ;;
  aarch64|arm64) arch="aarch64" ;;
  *) die "unsupported architecture: $(uname -m)" ;;
esac
ASSET="$BIN-$os-$arch.tar.gz"
dim "target: $os-$arch · release: $VERSION"

# --- download ---------------------------------------------------------------
TMP="$(mktemp -d 2>/dev/null || mktemp -d -t documentum)"
trap 'rm -rf "$TMP"' EXIT

if [ "$VERSION" = "latest" ]; then
  base="https://github.com/$REPO/releases/latest/download"
  api="https://api.github.com/repos/$REPO/releases/latest"
else
  base="https://github.com/$REPO/releases/download/$VERSION"
  api="https://api.github.com/repos/$REPO/releases/tags/$VERSION"
fi

# Public repo: plain download URLs. Private repo: asset ids through the API.
fetch() { # fetch <asset name> <dest>
  if [ -z "$TOKEN" ]; then
    curl -fsSL "$base/$1" -o "$2"
  else
    id="$(curl -fsSL -H "Authorization: Bearer $TOKEN" -H "Accept: application/vnd.github+json" "$api" \
          | tr -d '\n' | sed -E 's/\},\{/}\n{/g' | grep "\"name\":\"$1\"" | sed -E 's/.*"id":([0-9]+).*/\1/' | head -n1)"
    [ -n "$id" ] || return 1
    curl -fsSL -H "Authorization: Bearer $TOKEN" -H "Accept: application/octet-stream" \
      "https://api.github.com/repos/$REPO/releases/assets/$id" -o "$2"
  fi
}

dim "downloading $ASSET"
if ! fetch "$ASSET" "$TMP/$ASSET"; then
  if [ -z "$TOKEN" ]; then
    red "download failed: $base/$ASSET"
    dim "private fork or rate-limited? export GH_TOKEN=<token with repo scope> and retry"
  else
    red "download failed (no asset $ASSET in release $VERSION of $REPO)"
  fi
  if command -v cargo >/dev/null 2>&1; then
    dim "falling back to cargo install from source (a few minutes)…"
    cargo install --git "https://github.com/$REPO.git" --locked --root "$(dirname "$BINDIR")"
    ok "✓ built and installed: $BINDIR/$BIN"
    exit 0
  fi
  exit 1
fi

# --- verify -----------------------------------------------------------------
fetch "SHA256SUMS" "$TMP/SHA256SUMS" || die "release has no SHA256SUMS; refusing to install an unverified binary"
expected="$(awk -v n="$ASSET" '$2 == n || $2 == "*" n { print tolower($1) }' "$TMP/SHA256SUMS" | head -n1)"
[ -n "$expected" ] || die "SHA256SUMS has no entry for $ASSET"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$TMP/$ASSET" | awk '{print tolower($1)}')"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$TMP/$ASSET" | awk '{print tolower($1)}')"
else
  die "neither sha256sum nor shasum available to verify the download"
fi
[ "$actual" = "$expected" ] || die "checksum mismatch for $ASSET (expected $expected, got $actual)"
ok "✓ sha256 verified"

# --- install ----------------------------------------------------------------
tar -xzf "$TMP/$ASSET" -C "$TMP"
[ -f "$TMP/$BIN" ] || die "archive does not contain $BIN"
mkdir -p "$BINDIR"
if [ -w "$BINDIR" ]; then
  mv "$TMP/$BIN" "$BINDIR/$BIN"
else
  sudo mv "$TMP/$BIN" "$BINDIR/$BIN"
fi
chmod +x "$BINDIR/$BIN"
ok "✓ installed: $BINDIR/$BIN ($("$BINDIR/$BIN" --version 2>/dev/null || echo "$VERSION"))"

case ":$PATH:" in
  *":$BINDIR:"*) ;;
  *)
    red "note: $BINDIR is not on your PATH"
    dim "add it (then restart your shell):"
    printf '  echo '\''export PATH="%s:$PATH"'\'' >> ~/.profile\n' "$BINDIR"
    ;;
esac

printf '\nNext:\n'
dim "  cd my-workspace && documentum setup   # register the MCP server, index"
dim "  documentum search \"how do I deploy the api?\""
