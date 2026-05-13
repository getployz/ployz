#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PACKAGE_DIR="${DEPLOY_PACKAGE_DIR:-${ROOT_DIR}/packages/deploy}"
SCHEMA_PATH="${PACKAGE_DIR}/deploy-manifest.schema.json"
RUNTIME_SCHEMA_PATH="${PACKAGE_DIR}/runtime.schema.json"
TYPES_PATH="${PACKAGE_DIR}/index.d.ts"
RUNTIME_TYPES_PATH="${PACKAGE_DIR}/runtime.d.ts"
VERSION="$(bash "${ROOT_DIR}/scripts/read-workspace-version.sh")"
GENERATOR_VERSION="15.0.4"

mkdir -p "${PACKAGE_DIR}"

cargo run -p ployz-spec --example deploy_schema > "${SCHEMA_PATH}"
cargo run -p ployz-api --example runtime_schema > "${RUNTIME_SCHEMA_PATH}"

npm pkg set "version=${VERSION}" --prefix "${PACKAGE_DIR}" >/dev/null
npx --yes "json-schema-to-typescript@${GENERATOR_VERSION}" \
  "${SCHEMA_PATH}" \
  --bannerComment "" \
  --cwd "${ROOT_DIR}" \
  --unreachableDefinitions \
  --unknownAny false \
  -o "${TYPES_PATH}"

npx --yes "json-schema-to-typescript@${GENERATOR_VERSION}" \
  "${RUNTIME_SCHEMA_PATH}" \
  --bannerComment "" \
  --cwd "${ROOT_DIR}" \
  --unreachableDefinitions \
  --unknownAny false \
  -o "${RUNTIME_TYPES_PATH}"

printf '\nexport * from "./runtime";\n' >> "${TYPES_PATH}"
