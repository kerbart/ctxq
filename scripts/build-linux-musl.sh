#!/usr/bin/env bash
# Static Linux x86_64 (musl). C deps (ring, sqlite) need a musl-capable CC;
# the final link is rust-lld + the rustup musl sysroot (no musl-gcc required).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

HOST="$(rustc -vV | awk '/^host:/{print $2}')"
LLDDIR="$(rustc --print sysroot)/lib/rustlib/${HOST}/bin"
[ -x "$LLDDIR/rust-lld" ] || {
  echo "rust-lld missing at $LLDDIR/rust-lld — rustup target add x86_64-unknown-linux-musl" >&2
  exit 1
}

CC_WRAP=""
if command -v musl-gcc >/dev/null 2>&1; then
  CC_WRAP="$(command -v musl-gcc)"
  AR_WRAP="${AR:-ar}"
else
  ZIG=""
  if command -v zig >/dev/null 2>&1; then
    ZIG="$(command -v zig)"
  elif python3 -c 'import ziglang' >/dev/null 2>&1; then
    ZIG="python3 -m ziglang"
  elif [ -x /tmp/ctxq-zig/bin/python ]; then
    ZIG="/tmp/ctxq-zig/bin/python -m ziglang"
  fi
  [ -n "$ZIG" ] || {
    echo "need musl-gcc or zig (pip: python3 -m venv && pip install ziglang)" >&2
    exit 1
  }
  CC_WRAP="$(mktemp /tmp/ctxq-musl-cc.XXXXXX)"
  AR_WRAP="$(mktemp /tmp/ctxq-musl-ar.XXXXXX)"
  trap 'rm -f "$CC_WRAP" "$AR_WRAP"' EXIT
  cat >"$CC_WRAP" <<EOF
#!/usr/bin/env bash
out=(); skip=0
for a in "\$@"; do
  if [[ \$skip -eq 1 ]]; then skip=0; continue; fi
  case "\$a" in
    --target=*|-target) [[ "\$a" == "-target" ]] && skip=1 ;;
    *) out+=("\$a") ;;
  esac
done
exec $ZIG cc -target x86_64-linux-musl "\${out[@]}"
EOF
  cat >"$AR_WRAP" <<EOF
#!/usr/bin/env bash
exec $ZIG ar "\$@"
EOF
  chmod +x "$CC_WRAP" "$AR_WRAP"
fi

echo "CC=$CC_WRAP  linker=rust-lld  target=x86_64-unknown-linux-musl"
env PATH="$LLDDIR:$PATH" \
  CC_x86_64_unknown_linux_musl="$CC_WRAP" \
  AR_x86_64_unknown_linux_musl="$AR_WRAP" \
  CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
  CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static -C link-self-contained=yes" \
  cargo build --release --target x86_64-unknown-linux-musl "$@"
file "$ROOT/target/x86_64-unknown-linux-musl/release/ctxq"
