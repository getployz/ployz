#!/bin/sh
set -eu

default_version="v0.0.1-alpha.1"

usage() {
  echo "usage: [PLOYZ_VERSION=v0.0.1-alpha.1] sh ployz.sh [--version <version>] [--join-token <token>]" >&2
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --join-token)
      if [ "$#" -lt 2 ]; then
        usage
        exit 1
      fi
      if [ "${PLOYZ_JOIN_TOKEN:-}" ]; then
        echo "set join token as either --join-token or PLOYZ_JOIN_TOKEN, not both" >&2
        exit 1
      fi
      PLOYZ_JOIN_TOKEN="$2"
      shift 2
      ;;
    --version)
      if [ "$#" -lt 2 ]; then
        usage
        exit 1
      fi
      PLOYZ_VERSION="$2"
      shift 2
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

PLOYZ_VERSION="${PLOYZ_VERSION:-$default_version}"
case "$PLOYZ_VERSION" in
  v*)
    release_tag="$PLOYZ_VERSION"
    ;;
  *)
    release_tag="v$PLOYZ_VERSION"
    ;;
esac

if [ -z "$release_tag" ]; then
  echo "ployz version is empty" >&2
  exit 1
fi

case "$release_tag" in
  *[!A-Za-z0-9._-]*)
    echo "ployz version contains unsupported characters: $PLOYZ_VERSION" >&2
    exit 1
    ;;
esac

if [ "${PLOYZ_JOIN_TOKEN:-}" ] && [ -z "${PLOYZ_NATS_URL:-}" ]; then
  echo "set PLOYZ_NATS_URL when joining a machine" >&2
  exit 1
fi

if [ "$(uname -s)" != "Linux" ]; then
  echo "ployz requires Linux" >&2
  exit 1
fi

case "$(uname -m)" in
  x86_64 | amd64)
    release_platform="linux-amd64"
    ;;
  aarch64 | arm64)
    release_platform="linux-arm64"
    ;;
  *)
    echo "unsupported Linux architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

if [ "$(id -u)" -ne 0 ]; then
  echo "ployz installer must run as root" >&2
  exit 1
fi

command -v curl >/dev/null
command -v install >/dev/null
command -v sha256sum >/dev/null

manifest_url="${PLOYZ_RELEASE_MANIFEST_URL:-https://github.com/getployz/ployz/releases/download/${release_tag}/ployz-release-${release_platform}.env}"
install_dir="/usr/local/bin"
state_dir="/var/lib/ployz/keeper"
nats_dir="/var/lib/ployz/nats"
keeper_bin="${install_dir}/ployz-keeper"
ployzctl_bin="${install_dir}/ployzctl"
join_token_file="${state_dir}/join-token"
ca_file="${nats_dir}/ca.pem"
manifest_file="$(mktemp)"
tmp_file="$(mktemp)"
manifest_loaded=0

cleanup() {
  rm -f "$manifest_file" "$tmp_file"
}
trap cleanup EXIT

load_manifest() {
  if [ "$manifest_loaded" -eq 0 ]; then
    curl -fsSL "$manifest_url" -o "$manifest_file"
    manifest_loaded=1
  fi
}

manifest_value() {
  awk -F= -v key="$1" '$1 == key { print substr($0, length(key) + 2); exit }' "$manifest_file"
}

resolve_release_value() {
  name="$1"
  current="$2"
  if [ -n "$current" ]; then
    printf '%s\n' "$current"
    return 0
  fi

  load_manifest
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

  curl -fsSL "$url" -o "$target"
  printf '%s  %s\n' "$sha256" "$target" | sha256sum -c -
}

install -d -m 0755 "$install_dir"

if [ -z "${PLOYZ_JOIN_TOKEN:-}" ]; then
  PLOYZCTL_URL="$(resolve_release_value PLOYZCTL_URL "${PLOYZCTL_URL:-}")"
  PLOYZCTL_SHA256="$(resolve_release_value PLOYZCTL_SHA256 "${PLOYZCTL_SHA256:-}")"
  download_verified "$PLOYZCTL_URL" "$PLOYZCTL_SHA256" "$tmp_file"
  install -m 0755 "$tmp_file" "$ployzctl_bin"
  echo "installed $ployzctl_bin"
  exit 0
fi

PLOYZ_KEEPER_URL="$(resolve_release_value PLOYZ_KEEPER_URL "${PLOYZ_KEEPER_URL:-}")"
PLOYZ_KEEPER_SHA256="$(resolve_release_value PLOYZ_KEEPER_SHA256 "${PLOYZ_KEEPER_SHA256:-}")"
download_verified "$PLOYZ_KEEPER_URL" "$PLOYZ_KEEPER_SHA256" "$tmp_file"
install -d -m 0700 "$state_dir"
install -m 0755 "$tmp_file" "$keeper_bin"

# The cluster CA (public material) arrives base64-packed on the install
# command line; the keeper verifies the core's TLS NATS against it.
if [ "${PLOYZ_NATS_CA_B64:-}" ]; then
  command -v base64 >/dev/null
  install -d -m 0755 "$nats_dir"
  printf '%s' "$PLOYZ_NATS_CA_B64" | base64 -d > "$ca_file"
  PLOYZ_NATS_CA_FILE="$ca_file"
  export PLOYZ_NATS_CA_FILE
fi

umask 077
printf '%s\n' "$PLOYZ_JOIN_TOKEN" > "$join_token_file"

# PLOYZ_NATS_URL, PLOYZ_NATS_CA_FILE, and PLOYZ_JOIN_NKEY_SEED flow to the
# keeper, which redeems the join token with the low-privilege Join user.
"$keeper_bin" --join-token-file "$join_token_file"
