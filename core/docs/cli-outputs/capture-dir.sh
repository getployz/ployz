#!/usr/bin/env bash
# capture-dir.sh [PARENT]
# Creates and prints a fresh directory for a driver's raw captures, made with
# mktemp -d inside PARENT (default /tmp), so two drivers never share one.
# PARENT must sit outside any git checkout: drivers never write into the repo;
# publish.sh does that.
set -euo pipefail
shopt -s inherit_errexit
arg=${1:-/tmp}
parent=$(cd -- "$arg" && pwd) || { echo "capture-dir: $arg is not a directory" >&2; exit 1; }
if git -C "$parent" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "capture-dir: $parent is inside a git checkout; drivers never write into the repo" >&2
  exit 1
fi
path=$(mktemp -d "$parent/ployz-captures.XXXXXX") || exit 1
echo "$path"
