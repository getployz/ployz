#!/usr/bin/env bash

set -euo pipefail

# shellcheck source=scripts/release-archives.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/release-archives.sh"

checksum_for() {
    local checksums=$1 archive=$2 hash
    hash=$(awk -v archive="$archive" '$2 == archive { print $1; exit }' "$checksums")
    [ -n "$hash" ] || {
        echo "checksums.txt has no hash for $archive" >&2
        return 1
    }
    printf '%s\n' "$hash"
}

write_homebrew_formula_from_checksums() {
    local checksums=$1 dest=$2 version=$3 tag=$4 repo=$5
    local linux_amd64 linux_arm64 macos_amd64 macos_arm64
    local hash_linux_amd64 hash_linux_arm64 hash_macos_amd64 hash_macos_arm64
    linux_amd64=$(release_archive ployz linux amd64)
    linux_arm64=$(release_archive ployz linux arm64)
    macos_amd64=$(release_archive ployz macos amd64)
    macos_arm64=$(release_archive ployz macos arm64)
    hash_linux_amd64=$(checksum_for "$checksums" "$linux_amd64")
    hash_linux_arm64=$(checksum_for "$checksums" "$linux_arm64")
    hash_macos_amd64=$(checksum_for "$checksums" "$macos_amd64")
    hash_macos_arm64=$(checksum_for "$checksums" "$macos_arm64")
    mkdir -p "$(dirname "$dest")"
    cat > "$dest" <<EOF
# typed: false
# frozen_string_literal: true

class Ployz < Formula
  desc "Ployz CLI"
  homepage "https://github.com/${repo}"
  version "$version"

  on_macos do
    if Hardware::CPU.intel?
      url "https://github.com/${repo}/releases/download/${tag}/${macos_amd64}"
      sha256 "$hash_macos_amd64"
    end
    if Hardware::CPU.arm?
      url "https://github.com/${repo}/releases/download/${tag}/${macos_arm64}"
      sha256 "$hash_macos_arm64"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      if Hardware::CPU.is_64_bit?
        url "https://github.com/${repo}/releases/download/${tag}/${linux_amd64}"
        sha256 "$hash_linux_amd64"
      end
    end
    if Hardware::CPU.arm?
      if Hardware::CPU.is_64_bit?
        url "https://github.com/${repo}/releases/download/${tag}/${linux_arm64}"
        sha256 "$hash_linux_arm64"
      end
    end
  end

  def install
    bin.install "ployz"
  end

end
EOF
}
