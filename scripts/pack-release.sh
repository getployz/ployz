#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
DIST=${1:-${DIST:-"$ROOT/dist"}}

fail() { echo "release packing failed: $1" >&2; exit 1; }

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

cli_archives=$(printf '%s\n' \
    ployz_linux_amd64.tar.gz \
    ployz_linux_arm64.tar.gz \
    ployz_macos_amd64.tar.gz \
    ployz_macos_arm64.tar.gz)
all_archives=$(printf '%s\n' \
    $cli_archives \
    ployzd_linux_amd64.tar.gz \
    ployzd_linux_arm64.tar.gz)

for archive in $all_archives; do
    [ -f "$DIST/$archive" ] || fail "missing $archive"
done

version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)
[ -n "$version" ] || fail "workspace version is missing"
tag=${GITHUB_REF_NAME:-v$version}
[[ "$tag" == v* ]] || tag="v$tag"
repo=${GITHUB_REPOSITORY:-getployz/ployz2}

(
    cd "$DIST"
    # shellcheck disable=SC2086
    sha256sum $cli_archives | sort -k2 > checksums.txt
)

hash_linux_amd64=$(sha256 "$DIST/ployz_linux_amd64.tar.gz")
hash_linux_arm64=$(sha256 "$DIST/ployz_linux_arm64.tar.gz")
hash_macos_amd64=$(sha256 "$DIST/ployz_macos_amd64.tar.gz")
hash_macos_arm64=$(sha256 "$DIST/ployz_macos_arm64.tar.gz")

mkdir -p "$DIST/homebrew"
cat > "$DIST/homebrew/ployz.rb" <<EOF
# typed: false
# frozen_string_literal: true

class Ployz < Formula
  desc "Ployz CLI"
  homepage "https://github.com/${repo}"
  version "$version"

  on_macos do
    if Hardware::CPU.intel?
      url "https://github.com/${repo}/releases/download/${tag}/ployz_macos_amd64.tar.gz"
      sha256 "$hash_macos_amd64"

      def install
        bin.install "ployz"
      end
    end
    if Hardware::CPU.arm?
      url "https://github.com/${repo}/releases/download/${tag}/ployz_macos_arm64.tar.gz"
      sha256 "$hash_macos_arm64"

      def install
        bin.install "ployz"
      end
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      if Hardware::CPU.is_64_bit?
        url "https://github.com/${repo}/releases/download/${tag}/ployz_linux_amd64.tar.gz"
        sha256 "$hash_linux_amd64"

        def install
          bin.install "ployz"
        end
      end
    end
    if Hardware::CPU.arm?
      if Hardware::CPU.is_64_bit?
        url "https://github.com/${repo}/releases/download/${tag}/ployz_linux_arm64.tar.gz"
        sha256 "$hash_linux_arm64"

        def install
          bin.install "ployz"
        end
      end
    end
  end

  def caveats
    <<~EOS
      This formula replaces the older getployz/ployz implementation as a clean break
      with manual transition, not an in-place compatibility promise.
    EOS
  end
end
EOF
