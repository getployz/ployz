#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST_PATH="${ROOT_DIR}/crates/ployzd/assets/built_in_images.toml"
OUTPUT_DIR="${ROOT_DIR}/packaging/e2e/preloaded-images"

usage() {
  cat <<'EOF'
Usage:
  scripts/export-e2e-preloaded-images.sh [--output PATH]
EOF
}

manifest_value() {
  local key=$1
  awk -F'"' -v key="${key}" '$1 ~ ("^" key " = ") { print $2; exit }' "${MANIFEST_PATH}"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output)
      OUTPUT_DIR="${2:-}"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      printf 'unknown argument: %s\n' "$1" >&2
      exit 1
      ;;
  esac
done

mkdir -p "${OUTPUT_DIR}"

tmp_dir="$(mktemp -d "${OUTPUT_DIR}/.tmp.XXXXXX")"
trap 'rm -rf "${tmp_dir}"' EXIT

{
  for key in networking corrosion dns gateway; do
    ref="$(manifest_value "${key}")"
    if [[ -z "${ref}" ]]; then
      printf 'missing %s in %s\n' "${key}" "${MANIFEST_PATH}" >&2
      exit 1
    fi

    local_ref="ployz-e2e-preload/${key}:latest"
    tar_path="${tmp_dir}/${key}.tar"

    printf 'preload export start key=%s ref=%s\n' "${key}" "${ref}" >&2
    docker pull "${ref}" >/dev/null
    image_id="$(docker image inspect --format '{{.Id}}' "${ref}")"
    docker tag "${image_id}" "${local_ref}"
    docker save -o "${tar_path}" "${local_ref}"
    printf 'preload export complete key=%s tar=%s\n' "${key}" "${tar_path}" >&2
    printf '%s = "%s"\n' "${key}" "${local_ref}"
  done
} > "${tmp_dir}/built_in_images.toml"

rm -f "${OUTPUT_DIR}"/*.tar "${OUTPUT_DIR}/built_in_images.toml"
mv "${tmp_dir}"/*.tar "${OUTPUT_DIR}/"
mv "${tmp_dir}/built_in_images.toml" "${OUTPUT_DIR}/built_in_images.toml"
trap - EXIT
rm -rf "${tmp_dir}"
