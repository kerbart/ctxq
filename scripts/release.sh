#!/usr/bin/env bash
# Cut a release: tag the current commit and push the tag. GitHub Actions
# (.github/workflows/release.yml) builds every platform and publishes the
# archives + SHA256SUMS; install.sh / install.ps1 pick them up.
#
#   scripts/release.sh v2.0.1
set -euo pipefail

TAG="${1:-}"
if [ -z "$TAG" ]; then
  echo "usage: scripts/release.sh vX.Y.Z" >&2
  exit 1
fi
case "$TAG" in v[0-9]*) ;; *) echo "tag must look like v2.0.1" >&2; exit 1 ;; esac

cd "$(git rev-parse --show-toplevel)"
if [ -n "$(git status --porcelain)" ]; then
  echo "working tree is dirty — commit first" >&2
  exit 1
fi
CARGO_VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
if [ "v$CARGO_VERSION" != "$TAG" ]; then
  echo "Cargo.toml says $CARGO_VERSION but tag is $TAG — bump Cargo.toml (and Cargo.lock) first" >&2
  exit 1
fi

git tag -a "$TAG" -m "documentum $TAG"
git push origin "$TAG"
echo "==> tag $TAG pushed; follow the build: gh run watch --repo \"$(gh repo view --json nameWithOwner -q .nameWithOwner)\""
