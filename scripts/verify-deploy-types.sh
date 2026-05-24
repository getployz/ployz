#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PACKAGE_DIR="${ROOT_DIR}/packages/deploy"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

if ! cargo metadata --format-version 1 --no-deps \
  | grep -q '"name":"ployz-types"'; then
  echo "ployz-types is not part of this workspace; skipping deploy type verification"
  exit 0
fi

cp "${PACKAGE_DIR}/package.json" "${TMP_DIR}/package.json"
DEPLOY_PACKAGE_DIR="${TMP_DIR}" bash "${ROOT_DIR}/scripts/generate-deploy-types.sh" >/dev/null

for artifact in \
  package.json \
  deploy-manifest.schema.json \
  runtime.schema.json \
  index.d.ts \
  runtime.d.ts
do
  diff -u \
    --label "packages/deploy/${artifact}" \
    --label "generated/${artifact}" \
    "${PACKAGE_DIR}/${artifact}" \
    "${TMP_DIR}/${artifact}"
done
