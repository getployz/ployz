#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out_dir="${PLOYZ_SH_SITE_DIR:-${ROOT_DIR}/dist/ployz-sh-site}"

rm -rf "${out_dir}"
mkdir -p "${out_dir}/channels"

install -m 0644 "${ROOT_DIR}/scripts/ployz.sh" "${out_dir}/index.html"
install -m 0644 "${ROOT_DIR}/scripts/ployz.sh" "${out_dir}/install.sh"

if [ -f "${ROOT_DIR}/site/.nojekyll" ]; then
  install -m 0644 "${ROOT_DIR}/site/.nojekyll" "${out_dir}/.nojekyll"
else
  : > "${out_dir}/.nojekyll"
fi

find "${ROOT_DIR}/site/channels" -type f -name '*.env' -print | while IFS= read -r channel_file; do
  install -m 0644 "${channel_file}" "${out_dir}/channels/$(basename "${channel_file}")"
done

echo "ployz.sh site staged in ${out_dir}"
