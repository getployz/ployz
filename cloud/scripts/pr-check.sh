#!/usr/bin/env bash
set -u

stage=${1:-all}
checks=(
  "actions:actionlint ../.github/workflows/*.yml"
  "typecheck:pnpm typecheck"
  "lint:pnpm lint"
  "sdk:bash ../scripts/build-cloud-sdk.sh"
  "build:pnpm exec vite build && node scripts/package-sdk.mjs"
  "test:pnpm test"
)

failed=0
matched=0

for check in "${checks[@]}"; do
  name="${check%%:*}"
  command="${check#*:}"
  if [ "$stage" != all ] && [ "$stage" != "$name" ]; then
    continue
  fi
  matched=1
  started=$SECONDS

  printf 'Starting %s: %s\n' "$name" "$command"
  if bash -c "$command"; then
    printf 'Passed %s in %ss\n' "$name" "$((SECONDS - started))"
  else
    status="$?"
    printf 'Failed %s with exit code %s\n' "$name" "$status" >&2
    failed=1
  fi
done

[ "$matched" = 1 ] || { echo "Unknown check: $stage" >&2; exit 2; }
exit "$failed"
