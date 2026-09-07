#!/usr/bin/env bash
set -u

checks=(
  "actions:actionlint ../.github/workflows/cloud.yml"
  "typecheck:pnpm typecheck"
  "lint:pnpm lint"
  "build:pnpm build"
  "test:pnpm test"
)

failed=0

for check in "${checks[@]}"; do
  name="${check%%:*}"
  command="${check#*:}"

  printf 'Starting %s: %s\n' "$name" "$command"
  if bash -c "$command"; then
    printf 'Passed %s\n' "$name"
  else
    status="$?"
    printf 'Failed %s with exit code %s\n' "$name" "$status" >&2
    failed=1
  fi
done

exit "$failed"
