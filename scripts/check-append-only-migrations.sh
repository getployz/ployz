#!/usr/bin/env bash
# Migrations are append-only: fail when BASE..HEAD modifies or deletes an existing
# Cloud (drizzle) or Config Store migration. Adding a new one is fine.
set -euo pipefail

base=${1:-}
head=${2:-HEAD}
if [[ -z $base || $base =~ ^0+$ ]]; then
  echo "No base commit; nothing to compare."
  exit 0
fi

# --no-renames reports a move as a delete plus an add; the lowercase filter drops adds.
edited=$(git diff --name-status --no-renames --diff-filter=a "$base" "$head" -- \
  dashboard/drizzle \
  'core/crates/ployz-store/src/storage/migrations/*.sql')
if [[ -n $edited ]]; then
  echo "Migrations are append-only. Add a new migration instead of changing these:" >&2
  echo "$edited" >&2
  exit 1
fi
echo "No existing migration was modified or deleted."
