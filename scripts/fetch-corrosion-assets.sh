#!/usr/bin/env bash
set -euo pipefail

repo="${CORROSION_REPO:-getployz/corrosion}"
version="${1:-${CORROSION_VERSION:-}}"
bundle_dir="${2:-.bundle/corrosion}"

if [[ -z "${version}" && -f ".corrosion-version" ]]; then
	version="$(tr -d '[:space:]' < .corrosion-version)"
fi

if [[ -z "${version}" ]]; then
	echo "[corrosion-bundle] missing version: pass as arg, set CORROSION_VERSION, or add .corrosion-version" >&2
	exit 1
fi

if ! command -v gh >/dev/null 2>&1; then
	echo "[corrosion-bundle] gh CLI is required" >&2
	exit 1
fi

checksum_file() {
	local file_path
	file_path="$1"
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$file_path" | awk '{print $1}'
		return
	fi
	if command -v shasum >/dev/null 2>&1; then
		shasum -a 256 "$file_path" | awk '{print $1}'
		return
	fi
	echo "[corrosion-bundle] sha256sum or shasum is required" >&2
	exit 1
}

asset_specs=(
	"corrosion-x86_64-unknown-linux-gnu.tar.gz:linux_amd64"
	"corrosion-aarch64-unknown-linux-gnu.tar.gz:linux_arm64"
	"corrosion-x86_64-apple-darwin.tar.gz:darwin_amd64"
	"corrosion-aarch64-apple-darwin.tar.gz:darwin_arm64"
)

tmp_dir="${bundle_dir}/dist"
checksums_path="${tmp_dir}/checksums.txt"

rm -rf "${bundle_dir}"
mkdir -p "${tmp_dir}"

echo "[corrosion-bundle] downloading checksums from ${repo}@${version}"
if ! gh release download "${version}" -R "${repo}" --pattern "*checksums*.txt" --output "${checksums_path}"; then
	echo "[corrosion-bundle] release ${repo}@${version} must include a checksums.txt asset" >&2
	exit 1
fi

for spec in "${asset_specs[@]}"; do
	IFS=":" read -r asset target <<<"${spec}"
	asset_path="${tmp_dir}/${asset}"
	echo "[corrosion-bundle] downloading ${asset}"
	gh release download "${version}" -R "${repo}" --pattern "${asset}" --output "${asset_path}"

	expected="$(awk -v name="${asset}" '$2 == name { print $1; exit }' "${checksums_path}")"
	if [[ -z "${expected}" ]]; then
		echo "[corrosion-bundle] missing checksum entry for ${asset}" >&2
		exit 1
	fi

	actual="$(checksum_file "${asset_path}")"
	if [[ "${actual}" != "${expected}" ]]; then
		echo "[corrosion-bundle] checksum mismatch for ${asset}: expected ${expected}, got ${actual}" >&2
		exit 1
	fi

	extract_dir="${tmp_dir}/${target}"
	mkdir -p "${extract_dir}"
	tar -xzf "${asset_path}" -C "${extract_dir}"

	if [[ ! -f "${extract_dir}/corrosion" ]]; then
		echo "[corrosion-bundle] ${asset} did not contain a corrosion binary at archive root" >&2
		exit 1
	fi

	mkdir -p "${bundle_dir}/${target}"
	mv "${extract_dir}/corrosion" "${bundle_dir}/${target}/corrosion"
	chmod 0755 "${bundle_dir}/${target}/corrosion"
done

rm -rf "${tmp_dir}"
echo "[corrosion-bundle] prepared binaries under ${bundle_dir}"
