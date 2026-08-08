#!/bin/sh
set -eu

default_channel="alpha"
channel_base_url="https://ployz.sh/channels"

usage() {
  echo "usage: [PLOYZ_CHANNEL=alpha] sh ployz.sh [--channel <channel>] [--version <version>] [--build-executor]" >&2
  echo "" >&2
  echo "installs the verified ployz binary to /usr/local/bin/ployz" >&2
  echo "--build-executor also installs the verified Railpack helper" >&2
  echo "default install next step: sudo ployz init" >&2
}

version_input="${PLOYZ_VERSION:-}"
channel_input="${PLOYZ_CHANNEL:-}"
install_build_executor=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      if [ "$#" -lt 2 ]; then
        usage
        exit 1
      fi
      PLOYZ_VERSION="$2"
      version_input="$2"
      shift 2
      ;;
    --channel)
      if [ "$#" -lt 2 ]; then
        usage
        exit 1
      fi
      PLOYZ_CHANNEL="$2"
      channel_input="$2"
      shift 2
      ;;
    --build-executor)
      install_build_executor=1
      shift
      ;;
    -*)
      echo "unknown ployz installer argument: $1" >&2
      exit 1
      ;;
    *)
      usage
      exit 1
      ;;
  esac
done

if [ -z "${PLOYZ_RELEASE_MANIFEST_URL:-}" ] && [ -n "$version_input" ] && [ -n "$channel_input" ]; then
  echo "pass either --version/PLOYZ_VERSION or --channel/PLOYZ_CHANNEL, not both" >&2
  exit 1
fi

os_name="$(uname -s)"
case "$os_name" in
  Linux)
    os_slug="linux"
    ;;
  *)
    echo "unsupported operating system: $os_name (ployz bootstrap delivery requires Linux)" >&2
    exit 1
    ;;
esac

machine_arch="$(uname -m)"
case "$machine_arch" in
  x86_64 | amd64)
    arch_slug="amd64"
    railpack_binary_sha256="15d3921fc4f955f60b0d4b635036153c6255928ef0a6af7353e3047a2ceb6121"
    ;;
  aarch64 | arm64)
    arch_slug="arm64"
    railpack_binary_sha256="61723cee03fedaad6879c59729b1dabba329a4912573d77c37427149a070f8f9"
    ;;
  *)
    echo "unsupported architecture: $machine_arch (ployz supports amd64 and arm64)" >&2
    exit 1
    ;;
esac

release_platform="${os_slug}-${arch_slug}"

command -v install >/dev/null || {
  echo "ployz installer requires the install command" >&2
  exit 1
}
if [ "$(id -u)" -ne 0 ]; then
  command -v sudo >/dev/null || {
    echo "ployz installer requires sudo to install /usr/local/bin/ployz" >&2
    exit 1
  }
fi
if command -v sha256sum >/dev/null 2>&1; then
  sha256_tool="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
  sha256_tool="shasum"
else
  echo "ployz installer requires sha256sum or shasum" >&2
  exit 1
fi

install_dir="/usr/local/bin"
ployz_bin="${install_dir}/ployz"
railpack_version="v0.31.0"
railpack_bin="/usr/local/lib/ployz/railpack/v0.31.0/railpack"
release_env_file="${PLOYZ_RELEASE_ENV_FILE:-/etc/ployz/release.env}"
staging_dir="$(mktemp -d)"
manifest_file="${staging_dir}/release.env.remote"
channel_file="${staging_dir}/channel.env"
ployz_stage="${staging_dir}/ployz"
railpack_stage="${staging_dir}/railpack"
release_env_stage="${staging_dir}/release.env"

cleanup() {
  rm -rf "$staging_dir"
}
trap cleanup EXIT

env_value() {
  file="$1"
  key="$2"
  awk -F= -v key="$key" '$1 == key { print substr($0, length(key) + 2); exit }' "$file"
}

fetch_file() {
  source="$1"
  target="$2"
  case "$source" in
    file://*) cp "${source#file://}" "$target" ;;
    /*) cp "$source" "$target" ;;
    *)
      command -v curl >/dev/null || {
        echo "ployz installer requires curl to download $source" >&2
        exit 1
      }
      curl -fsSL --retry 3 --retry-delay 1 --retry-max-time 60 --retry-connrefused --connect-timeout 10 --max-time 30 "$source" -o "$target"
      ;;
  esac
}

validate_token() {
  name="$1"
  value="$2"
  if [ -z "$value" ]; then
    echo "$name is empty" >&2
    exit 1
  fi
  case "$value" in
    *[!A-Za-z0-9._-]*)
      echo "$name contains unsupported characters: $value" >&2
      exit 1
      ;;
  esac
}

validate_railpack_version() {
  value="$1"
  validate_token "Railpack version" "$value"
  if ! awk -v value="$value" 'BEGIN { exit(value ~ /^v[0-9]+\.[0-9]+\.[0-9]+$/ ? 0 : 1) }'; then
    echo "Railpack version must be an exact vMAJOR.MINOR.PATCH token: $value" >&2
    exit 1
  fi
}

github_release_base_url() {
  printf 'https://github.com/getployz/ployz/releases/download/%s\n' "$1"
}

normalize_release_version() {
  raw_version="$1"
  validate_token "ployz version" "$raw_version"
  case "$raw_version" in
    v*)
      release_tag="$raw_version"
      PLOYZ_VERSION="${raw_version#v}"
      ;;
    *)
      release_tag="v$raw_version"
      PLOYZ_VERSION="$raw_version"
      ;;
  esac
}

channel_value() {
  env_value "$channel_file" "$1"
}

resolve_channel() {
  selected_channel="${PLOYZ_CHANNEL:-$default_channel}"
  validate_token "ployz channel" "$selected_channel"
  channel_url="${channel_base_url}/${selected_channel}.env"

  if ! fetch_file "$channel_url" "$channel_file"; then
    echo "failed to download release channel $channel_url" >&2
    exit 1
  fi

  channel_release_tag="$(channel_value PLOYZ_RELEASE_TAG)"
  if [ -z "$channel_release_tag" ]; then
    echo "release channel $channel_url is missing PLOYZ_RELEASE_TAG" >&2
    exit 1
  fi
  channel_version="$(channel_value PLOYZ_VERSION)"
  if [ -z "$channel_version" ]; then
    echo "release channel $channel_url is missing PLOYZ_VERSION" >&2
    exit 1
  fi
  channel_release_base_url="$(channel_value PLOYZ_RELEASE_BASE_URL)"
  if [ -z "$channel_release_base_url" ]; then
    echo "release channel $channel_url is missing PLOYZ_RELEASE_BASE_URL" >&2
    exit 1
  fi

  validate_token "ployz release tag" "$channel_release_tag"
  validate_token "ployz version" "$channel_version"
  release_tag="$channel_release_tag"
  PLOYZ_VERSION="$channel_version"
  expected_release_base_url="$(github_release_base_url "$release_tag")"
  if [ "${channel_release_base_url%/}" != "$expected_release_base_url" ]; then
    echo "release channel $channel_url has PLOYZ_RELEASE_BASE_URL=$channel_release_base_url, expected $expected_release_base_url" >&2
    exit 1
  fi
  manifest_url="${expected_release_base_url}/ployz-release-${release_platform}.env"
  echo "resolved ployz channel ${selected_channel} -> ${release_tag}"
}

caller_manifest_selected=0
if [ -n "${PLOYZ_RELEASE_MANIFEST_URL:-}" ]; then
  caller_manifest_selected=1
  manifest_url="$PLOYZ_RELEASE_MANIFEST_URL"
  if [ -n "$version_input" ]; then
    normalize_release_version "$version_input"
  fi
elif [ -n "$version_input" ]; then
  normalize_release_version "$version_input"
  manifest_url="$(github_release_base_url "$release_tag")/ployz-release-${release_platform}.env"
else
  resolve_channel
fi

load_manifest() {
  if ! fetch_file "$manifest_url" "$manifest_file"; then
    echo "failed to download release manifest $manifest_url" >&2
    exit 1
  fi
  verify_release_manifest_identity
}

manifest_value() {
  env_value "$manifest_file" "$1"
}

verify_release_manifest_identity() {
  manifest_tag="$(manifest_value PLOYZ_RELEASE_TAG)"
  if [ -z "$manifest_tag" ]; then
    echo "release manifest $manifest_url is missing PLOYZ_RELEASE_TAG" >&2
    exit 1
  fi
  validate_token "ployz release tag" "$manifest_tag"
  if [ -n "${release_tag:-}" ] && [ "$manifest_tag" != "$release_tag" ]; then
    echo "release manifest $manifest_url has PLOYZ_RELEASE_TAG=$manifest_tag, expected $release_tag" >&2
    exit 1
  fi

  manifest_version="$(manifest_value PLOYZ_VERSION)"
  if [ -z "$manifest_version" ]; then
    echo "release manifest $manifest_url is missing PLOYZ_VERSION" >&2
    exit 1
  fi
  validate_token "ployz version" "$manifest_version"
  if [ "$manifest_tag" != "v$manifest_version" ]; then
    echo "release manifest $manifest_url has incoherent identity: PLOYZ_RELEASE_TAG=$manifest_tag and PLOYZ_VERSION=$manifest_version" >&2
    exit 1
  fi
  if [ -n "${PLOYZ_VERSION:-}" ] && [ "$manifest_version" != "$PLOYZ_VERSION" ]; then
    echo "release manifest $manifest_url has PLOYZ_VERSION=$manifest_version, expected $PLOYZ_VERSION" >&2
    exit 1
  fi

  manifest_platform="$(manifest_value PLOYZ_RELEASE_PLATFORM)"
  if [ -z "$manifest_platform" ]; then
    echo "release manifest $manifest_url is missing PLOYZ_RELEASE_PLATFORM" >&2
    exit 1
  fi
  if [ "$manifest_platform" != "$release_platform" ]; then
    echo "release manifest $manifest_url has PLOYZ_RELEASE_PLATFORM=$manifest_platform, expected $release_platform" >&2
    exit 1
  fi

  release_tag="$manifest_tag"
  PLOYZ_VERSION="$manifest_version"
}

manifest_required_value() {
  name="$1"
  value="$(manifest_value "$name")"
  if [ -z "$value" ]; then
    echo "release manifest $manifest_url is missing $name" >&2
    exit 1
  fi
  printf '%s\n' "$value"
}

download_verified() {
  url="$1"
  sha256="$2"
  target="$3"

  fetch_file "$url" "$target"
  case "$sha256_tool" in
    sha256sum)
      printf '%s  %s\n' "$sha256" "$target" | sha256sum -c - >&2
      ;;
    shasum)
      printf '%s  %s\n' "$sha256" "$target" | shasum -a 256 -c - >&2
      ;;
  esac
}

install_checked() {
  if [ "$(id -u)" -eq 0 ]; then
    install "$@"
  else
    sudo install "$@"
  fi
}

stage_release_env() {
  {
    printf 'PLOYZ_RELEASE_MANIFEST_URL=%s\n' "$manifest_url"
    printf 'PLOYZ_VERSION=%s\n' "${PLOYZ_VERSION:-}"
    printf 'PLOYZ_RELEASE_TAG=%s\n' "${release_tag:-}"
    printf 'PLOYZ_RELEASE_PLATFORM=%s\n' "$release_platform"
  } > "$release_env_stage"
}

if [ "$install_build_executor" -eq 1 ]; then
  if [ "${PLOYZ_URL+x}" = x ] || [ "${PLOYZ_SHA256+x}" = x ] || \
     [ "${PLOYZ_RAILPACK_VERSION+x}" = x ] || [ "${PLOYZ_RAILPACK_URL+x}" = x ] || \
     [ "${PLOYZ_RAILPACK_SHA256+x}" = x ]; then
    echo "--build-executor requires both artifacts to come from the resolved release manifest; direct artifact overrides are not supported" >&2
    exit 1
  fi
  load_manifest
  PLOYZ_URL="$(manifest_required_value PLOYZ_URL)"
  PLOYZ_SHA256="$(manifest_required_value PLOYZ_SHA256)"
  PLOYZ_RAILPACK_VERSION="$(manifest_required_value PLOYZ_RAILPACK_VERSION)"
  validate_railpack_version "$PLOYZ_RAILPACK_VERSION"
  if [ "$PLOYZ_RAILPACK_VERSION" != "$railpack_version" ]; then
    echo "release manifest $manifest_url has unsupported PLOYZ_RAILPACK_VERSION=$PLOYZ_RAILPACK_VERSION, expected $railpack_version" >&2
    exit 1
  fi
  PLOYZ_RAILPACK_URL="$(manifest_required_value PLOYZ_RAILPACK_URL)"
  PLOYZ_RAILPACK_SHA256="$(manifest_required_value PLOYZ_RAILPACK_SHA256)"
  if [ "$PLOYZ_RAILPACK_SHA256" != "$railpack_binary_sha256" ]; then
    echo "release manifest $manifest_url has unsupported PLOYZ_RAILPACK_SHA256=$PLOYZ_RAILPACK_SHA256, expected $railpack_binary_sha256 for $release_platform" >&2
    exit 1
  fi
else
  if [ "$caller_manifest_selected" -eq 1 ] || [ -z "${PLOYZ_URL:-}" ] || [ -z "${PLOYZ_SHA256:-}" ]; then
    load_manifest
  fi
  PLOYZ_URL="${PLOYZ_URL:-$(manifest_required_value PLOYZ_URL)}"
  PLOYZ_SHA256="${PLOYZ_SHA256:-$(manifest_required_value PLOYZ_SHA256)}"
fi

download_verified "$PLOYZ_URL" "$PLOYZ_SHA256" "$ployz_stage"
if [ "$install_build_executor" -eq 1 ]; then
  download_verified "$PLOYZ_RAILPACK_URL" "$PLOYZ_RAILPACK_SHA256" "$railpack_stage"
fi
stage_release_env

promote_release() {
  install_checked -d -m 0755 "$install_dir"
  if [ "$install_build_executor" -eq 1 ]; then
    railpack_dir="${railpack_bin%/*}"
    install_checked -d -m 0755 "$railpack_dir"
    install_checked -m 0755 "$railpack_stage" "$railpack_bin"
  fi
  install_checked -m 0755 "$ployz_stage" "$ployz_bin"
  release_env_dir="${release_env_file%/*}"
  install_checked -d -m 0755 "$release_env_dir"
  install_checked -m 0644 "$release_env_stage" "$release_env_file"
}

promote_release

echo "installed $ployz_bin"
if [ "$install_build_executor" -eq 1 ]; then
  echo "installed $railpack_bin"
  echo "ready for Build Executor enrollment"
else
  echo "run: sudo ployz init"
fi
