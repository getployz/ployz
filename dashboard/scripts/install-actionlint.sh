#!/usr/bin/env bash
set -euo pipefail

install_dir="${1:?usage: install-actionlint.sh INSTALL_DIR}"
version="1.7.12"
checksum="8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8"
archive="actionlint_${version}_linux_amd64.tar.gz"
url="https://github.com/rhysd/actionlint/releases/download/v${version}/${archive}"

temporary_dir="$(mktemp -d)"
trap 'rm -rf "$temporary_dir"' EXIT

curl -fsSL "$url" -o "$temporary_dir/$archive"
printf '%s  %s\n' "$checksum" "$temporary_dir/$archive" | sha256sum -c -
tar -xzf "$temporary_dir/$archive" -C "$temporary_dir" actionlint

mkdir -p "$install_dir"
install -m 755 "$temporary_dir/actionlint" "$install_dir/actionlint"
