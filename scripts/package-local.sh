#!/usr/bin/env bash
# Package Linux x86_64 into dist/releases/ for ctxq.dev.
# Prefers the musl static binary when present.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MUSL="$ROOT/target/x86_64-unknown-linux-musl/release/ctxq"
GNU="$ROOT/target/release/ctxq"
if [ -x "$MUSL" ]; then
  SRC="$MUSL"
  KIND=musl
else
  [ -x "$GNU" ] || { echo "missing $GNU — cargo build --release first (or scripts/build-linux-musl.sh)" >&2; exit 1; }
  SRC="$GNU"
  KIND=gnu
fi

ASSET="ctxq-linux-x86_64.tar.gz"
OUT="$ROOT/dist/releases"
mkdir -p "$OUT"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp "$SRC" "$stage/ctxq"
chmod 755 "$stage/ctxq"
if command -v strip >/dev/null; then
  strip --strip-unneeded "$stage/ctxq" 2>/dev/null || true
fi
echo "packaging $KIND $($stage/ctxq --version | head -1)"
file "$stage/ctxq"
tar -czf "$OUT/$ASSET" -C "$stage" ctxq
(cd "$OUT" && sha256sum "$ASSET" > SHA256SUMS.tmp && {
  if [ -f SHA256SUMS ]; then
    grep -v " $ASSET\$" SHA256SUMS || true
    cat SHA256SUMS.tmp
  else
    cat SHA256SUMS.tmp
  fi
} > SHA256SUMS.new && mv SHA256SUMS.new SHA256SUMS && rm -f SHA256SUMS.tmp)
echo "wrote $OUT/$ASSET ($KIND)"
cat "$OUT/SHA256SUMS"
