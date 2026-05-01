#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/payload-stamp.sh <repo_root> <target_platform> <build_profile>

Emits a content-addressed hash that ployz-e2e uses to decide whether the
payload cache (release binaries built by build-install-payload.sh) is
still fresh. Both runner.rs and CI invoke this so the values match.
EOF
}

if [[ $# -ne 3 ]]; then
  usage >&2
  exit 1
fi

repo_root=$1
target_platform=$2
build_profile=$3

hash_cmd() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256
    return
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum
    return
  fi
  cksum
}

{
  printf 'platform=%s\n' "${target_platform}"
  printf 'profile=%s\n' "${build_profile}"
  {
    git -C "${repo_root}" ls-files -z -- Cargo.toml Cargo.lock .corrosion-version .nats-version ployz.sh crates scripts packaging
    git -C "${repo_root}" ls-files --others --exclude-standard -z -- Cargo.toml Cargo.lock .corrosion-version .nats-version ployz.sh crates scripts packaging
  } | LC_ALL=C sort -zu \
    | while IFS= read -r -d '' path; do
        printf 'path=%s\n' "${path}"
        if [[ -f "${repo_root}/${path}" ]]; then
          cat "${repo_root}/${path}"
        else
          printf '<missing>\n'
        fi
        printf '\n'
      done
} | hash_cmd | awk '{print $1}'
