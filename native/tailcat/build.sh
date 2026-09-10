#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
PLOYZ_VERSION=${PLOYZ_VERSION:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' ../../Cargo.toml | head -1)}
revision=91dc4979bd4ae88af6ae2c8bb549616de4bcaa5a
# Keep the upstream tree disposable; local changes belong in lifecycle.patch.
if [[ ! -d upstream/.git ]]; then
  git init -q upstream
  git -C upstream remote add origin https://github.com/tailscale/tailcat.git
fi
if ! git -C upstream cat-file -e "$revision^{commit}" 2>/dev/null; then
  git -C upstream fetch --depth=1 origin "$revision"
fi
git -C upstream reset --hard -q "$revision"
git -C upstream clean -fdq
git -C upstream apply ../lifecycle.patch
if [[ "${1:-}" == --test ]]; then
  (cd upstream && go test -run '^TestBoundedClientLifecycle$' -count=1 .)
  go test ./...
  if [[ -n "${2:-}" ]]; then
    mkdir -p "$(dirname "${2:-ployz-tailcat}")"
    go build -trimpath -ldflags "-X main.version=${PLOYZ_VERSION:-dev}" -o "$2" .
  fi
else
  mkdir -p "$(dirname "${1:-ployz-tailcat}")"
  go build -trimpath -ldflags "-X main.version=${PLOYZ_VERSION:-dev}" -o "${1:-ployz-tailcat}" .
fi
