#!/bin/sh
# Hostnames routed by a Cloudflare Tunnel, as a markdown table (for `kind = "command"`, `format = "markdown"`).
# Usage: cloudflared-domains.sh [config.yml]   (default: ~/.cloudflared/config.yml)
# Reads `ingress` rules with plain YAML parsing: no yq/python needed.
cfg="${1:-$HOME/.cloudflared/config.yml}"
if [ ! -f "$cfg" ]; then
  echo "_(cloudflared configuration not found: $cfg)_"
  exit 0
fi
echo "| Hostname | → service |"
echo "|---|---|"
awk '
  /^[[:space:]]*-?[[:space:]]*hostname:/ { sub(/^[^:]*:[[:space:]]*/, ""); gsub(/["'\'']/, ""); host = $0; next }
  /^[[:space:]]*-?[[:space:]]*service:/ {
    sub(/^[^:]*:[[:space:]]*/, ""); gsub(/["'\'']/, "")
    if (host != "") { printf "| `%s` | %s |\n", host, $0 }
    host = ""
  }
' "$cfg"
