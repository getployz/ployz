#!/bin/sh
set -eu

default_channel="alpha"
channel_base_url="https://ployz.sh/channels"

usage() {
  echo "usage: [PLOYZ_CHANNEL=alpha] sh ployz.sh [--channel <channel>] [--version <version>] [--join-token <token>] [--first-node]" >&2
  echo "" >&2
  echo "modes:" >&2
  echo "  (default)                install the local ployzctl CLI (macOS or Linux, no root needed)" >&2
  echo "  --join-token <token>     machine bootstrap: join this Linux machine to a cluster (root)" >&2
  echo "  --first-node             machine bootstrap: form a first node on this Linux machine (root)" >&2
}

version_input="${PLOYZ_VERSION:-}"
channel_input="${PLOYZ_CHANNEL:-}"

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
    --first-node)
      PLOYZ_FIRST_NODE=1
      shift
      ;;
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

# One install mode per invocation: the default installs the local operator
# CLI; the machine modes bootstrap a cluster machine through the keeper.
if [ "${PLOYZ_JOIN_TOKEN:-}" ] && [ "${PLOYZ_FIRST_NODE:-}" ]; then
  echo "pass either --join-token or --first-node, not both" >&2
  exit 1
fi

install_mode="local"
if [ "${PLOYZ_JOIN_TOKEN:-}" ]; then
  install_mode="join"
elif [ "${PLOYZ_FIRST_NODE:-}" ]; then
  install_mode="first-node"
fi

if [ "$install_mode" = "join" ] && [ -z "${PLOYZ_NATS_URL:-}" ]; then
  echo "set PLOYZ_NATS_URL when joining a machine" >&2
  exit 1
fi

if [ "$install_mode" = "first-node" ] && [ -z "${PLOYZ_NODE_ID:-}" ]; then
  echo "set PLOYZ_NODE_ID when bootstrapping a first node" >&2
  exit 1
fi

if [ "$install_mode" = "first-node" ] && [ -z "${PLOYZ_MACHINE_JOIN_NATS_URL:-}" ]; then
  echo "set PLOYZ_MACHINE_JOIN_NATS_URL when bootstrapping a first node" >&2
  exit 1
fi

os_name="$(uname -s)"
case "$os_name" in
  Linux)
    os_slug="linux"
    ;;
  Darwin)
    os_slug="darwin"
    ;;
  *)
    echo "unsupported operating system: $os_name (ployz supports Linux and macOS)" >&2
    exit 1
    ;;
esac

machine_arch="$(uname -m)"
case "$machine_arch" in
  x86_64 | amd64)
    arch_slug="amd64"
    ;;
  aarch64 | arm64)
    arch_slug="arm64"
    ;;
  *)
    echo "unsupported architecture: $machine_arch (ployz supports amd64 and arm64)" >&2
    exit 1
    ;;
esac

release_platform="${os_slug}-${arch_slug}"

if [ "$install_mode" != "local" ]; then
  if [ "$os_slug" != "linux" ]; then
    echo "ployz machine bootstrap requires Linux; this machine is $os_name" >&2
    exit 1
  fi
  if [ "$(id -u)" -ne 0 ]; then
    echo "ployz machine bootstrap must run as root" >&2
    exit 1
  fi
fi

command -v curl >/dev/null || {
  echo "ployz installer requires curl" >&2
  exit 1
}
command -v install >/dev/null || {
  echo "ployz installer requires the install command" >&2
  exit 1
}
if command -v sha256sum >/dev/null 2>&1; then
  sha256_tool="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
  sha256_tool="shasum"
else
  echo "ployz installer requires sha256sum or shasum" >&2
  exit 1
fi

install_dir="/usr/local/bin"
if [ "$install_mode" = "local" ] && [ "$(id -u)" -ne 0 ]; then
  install_dir="${HOME}/.local/bin"
fi
state_dir="/var/lib/ployz/keeper"
nats_dir="/var/lib/ployz/nats"
nats_version="2.14.2"
nats_binary="/usr/local/bin/nats-server"
nats_config="/etc/nats/nats-server.conf"
machine_join_template_file="/etc/ployz/machine-join-template.json"
keeper_bin="${install_dir}/ployz-keeper"
ployzctl_bin="${install_dir}/ployzctl"
join_token_file="${state_dir}/join-token"
ca_file="${nats_dir}/ca.pem"
manifest_file="$(mktemp)"
channel_file="$(mktemp)"
tmp_file="$(mktemp)"
first_node_spec_file="$(mktemp)"
manifest_loaded=0
release_manifest_identity_required=0

cleanup() {
  rm -f "$manifest_file" "$channel_file" "$tmp_file" "$first_node_spec_file"
}
trap cleanup EXIT

env_value() {
  file="$1"
  key="$2"
  awk -F= -v key="$key" '$1 == key { print substr($0, length(key) + 2); exit }' "$file"
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

  if ! curl -fsSL "$channel_url" -o "$channel_file"; then
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
  release_manifest_identity_required=1
  echo "resolved ployz channel ${selected_channel} -> ${release_tag}"
}

if [ -n "${PLOYZ_RELEASE_MANIFEST_URL:-}" ]; then
  manifest_url="$PLOYZ_RELEASE_MANIFEST_URL"
  if [ -n "$version_input" ]; then
    normalize_release_version "$version_input"
  fi
elif [ -n "$version_input" ]; then
  normalize_release_version "$version_input"
  manifest_url="$(github_release_base_url "$release_tag")/ployz-release-${release_platform}.env"
  release_manifest_identity_required=1
else
  resolve_channel
fi

load_manifest() {
  if [ "$manifest_loaded" -eq 0 ]; then
    if ! curl -fsSL "$manifest_url" -o "$manifest_file"; then
      echo "failed to download release manifest $manifest_url" >&2
      exit 1
    fi
    verify_release_manifest_identity
    manifest_loaded=1
  fi
}

manifest_value() {
  env_value "$manifest_file" "$1"
}

verify_release_manifest_identity() {
  if [ "$release_manifest_identity_required" -eq 0 ]; then
    return 0
  fi

  manifest_tag="$(manifest_value PLOYZ_RELEASE_TAG)"
  if [ -z "$manifest_tag" ]; then
    echo "release manifest $manifest_url is missing PLOYZ_RELEASE_TAG" >&2
    exit 1
  fi
  if [ "$manifest_tag" != "$release_tag" ]; then
    echo "release manifest $manifest_url has PLOYZ_RELEASE_TAG=$manifest_tag, expected $release_tag" >&2
    exit 1
  fi

  manifest_version="$(manifest_value PLOYZ_VERSION)"
  if [ -z "$manifest_version" ]; then
    echo "release manifest $manifest_url is missing PLOYZ_VERSION" >&2
    exit 1
  fi
  if [ "$manifest_version" != "$PLOYZ_VERSION" ]; then
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
  case "$sha256_tool" in
    sha256sum)
      printf '%s  %s\n' "$sha256" "$target" | sha256sum -c - >&2
      ;;
    shasum)
      printf '%s  %s\n' "$sha256" "$target" | shasum -a 256 -c - >&2
      ;;
  esac
}

sha256_file() {
  case "$sha256_tool" in
    sha256sum)
      sha256sum "$1" | awk '{ print $1 }'
      ;;
    shasum)
      shasum -a 256 "$1" | awk '{ print $1 }'
      ;;
  esac
}

expected_nats_server_archive_sha256() {
  case "$arch_slug" in
    amd64)
      printf 'b3e7b14eb10c895fd90c2dacdb6b65bd3208adcc9524dd7689ba2c1024e6b97a\n'
      ;;
    arm64)
      printf '15fd0c3438e7178e5316e63be68373ad581c8d78db26e649113aa303b74e5e58\n'
      ;;
    *)
      echo "unsupported architecture for nats-server: $arch_slug" >&2
      exit 1
      ;;
  esac
}

json_string() {
  value="$1"
  case "$value" in
    *\"* | *\\*)
      echo "first-node value contains unsupported JSON characters: $value" >&2
      exit 1
      ;;
  esac
  printf '"%s"' "$value"
}

json_optional_string() {
  value="${1:-}"
  if [ -z "$value" ]; then
    printf 'null'
  else
    json_string "$value"
  fi
}

validate_role_value() {
  case "$2" in
    install | skip)
      ;;
    *)
      echo "$1 must be install or skip" >&2
      exit 1
      ;;
  esac
}

ensure_nats_server() {
  if [ ! -x "$nats_binary" ]; then
    case "$arch_slug" in
      amd64 | arm64)
        ;;
      *)
        echo "unsupported architecture for nats-server: $arch_slug" >&2
        exit 1
        ;;
    esac
    nats_archive="/tmp/ployz-nats-server.tar.gz"
    if ! download_verified \
      "https://github.com/nats-io/nats-server/releases/download/v${nats_version}/nats-server-v${nats_version}-linux-${arch_slug}.tar.gz" \
      "$(expected_nats_server_archive_sha256)" \
      "$nats_archive"; then
      rm -f "$nats_archive"
      exit 1
    fi
    if ! tar -xzf "$nats_archive" -C /tmp; then
      rm -rf "$nats_archive" "/tmp/nats-server-v${nats_version}-linux-${arch_slug}"
      exit 1
    fi
    if ! install -m 0755 "/tmp/nats-server-v${nats_version}-linux-${arch_slug}/nats-server" "$nats_binary"; then
      rm -rf "$nats_archive" "/tmp/nats-server-v${nats_version}-linux-${arch_slug}"
      exit 1
    fi
    rm -rf "$nats_archive" "/tmp/nats-server-v${nats_version}-linux-${arch_slug}"
  fi
  sha256_file "$nats_binary"
}

write_first_node_spec() {
  if [ -z "${PLOYZ_NODE_ID:-}" ]; then
    echo "set PLOYZ_NODE_ID when bootstrapping a first node" >&2
    exit 1
  fi
  if [ -z "${PLOYZ_MACHINE_JOIN_NATS_URL:-}" ]; then
    echo "set PLOYZ_MACHINE_JOIN_NATS_URL when bootstrapping a first node" >&2
    exit 1
  fi
  PLOYZ_GATEWAY="${PLOYZ_GATEWAY:-install}"
  PLOYZ_DNS="${PLOYZ_DNS:-install}"
  PLOYZ_MACHINE_BOOTSTRAP_URL="${PLOYZ_MACHINE_BOOTSTRAP_URL:-https://ployz.sh}"
  PLOYZ_MACHINE_JOIN_CLUSTER_NAME="${PLOYZ_MACHINE_JOIN_CLUSTER_NAME:-ployz}"
  validate_role_value PLOYZ_GATEWAY "$PLOYZ_GATEWAY"
  validate_role_value PLOYZ_DNS "$PLOYZ_DNS"

  PLOYZD_URL="$(resolve_release_value PLOYZD_URL "${PLOYZD_URL:-}")"
  PLOYZD_SHA256="$(resolve_release_value PLOYZD_SHA256 "${PLOYZD_SHA256:-}")"
  PLOYZ_EBPF_TC_URL="$(resolve_release_value PLOYZ_EBPF_TC_URL "${PLOYZ_EBPF_TC_URL:-}")"
  PLOYZ_EBPF_TC_SHA256="$(resolve_release_value PLOYZ_EBPF_TC_SHA256 "${PLOYZ_EBPF_TC_SHA256:-}")"
  PLOYZ_EBPF_CTL_URL="$(resolve_release_value PLOYZ_EBPF_CTL_URL "${PLOYZ_EBPF_CTL_URL:-}")"
  PLOYZ_EBPF_CTL_SHA256="$(resolve_release_value PLOYZ_EBPF_CTL_SHA256 "${PLOYZ_EBPF_CTL_SHA256:-}")"
  PLOYZ_VERSION="$(resolve_release_value PLOYZ_VERSION "${PLOYZ_VERSION:-}")"
  NATS_SERVER_SHA256="$(ensure_nats_server)"

  cat > "$first_node_spec_file" <<EOF
{
  "node_id": $(json_string "$PLOYZ_NODE_ID"),
  "gateway": $(json_string "$PLOYZ_GATEWAY"),
  "dns": $(json_string "$PLOYZ_DNS"),
  "node_public_ip": $(json_optional_string "${PLOYZ_NODE_PUBLIC_IP:-}"),
  "machine_bootstrap_url": $(json_string "$PLOYZ_MACHINE_BOOTSTRAP_URL"),
  "machine_join_template_file": $(json_string "$machine_join_template_file"),
  "machine_join_cluster_name": $(json_string "$PLOYZ_MACHINE_JOIN_CLUSTER_NAME"),
  "machine_join_runtime_nats_url": $(json_string "$PLOYZ_MACHINE_JOIN_NATS_URL"),
  "artifacts": {
    "ployzd": {
      "version": $(json_string "$PLOYZ_VERSION"),
      "source": $(json_string "$PLOYZD_URL"),
      "sha256": $(json_string "$PLOYZD_SHA256"),
      "install_path": "/usr/local/bin/ployzd"
    },
    "ebpf_bytecode": {
      "version": $(json_string "$PLOYZ_VERSION"),
      "source": $(json_string "$PLOYZ_EBPF_TC_URL"),
      "sha256": $(json_string "$PLOYZ_EBPF_TC_SHA256"),
      "install_path": "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"
    },
    "ebpf_ctl": {
      "version": $(json_string "$PLOYZ_VERSION"),
      "source": $(json_string "$PLOYZ_EBPF_CTL_URL"),
      "sha256": $(json_string "$PLOYZ_EBPF_CTL_SHA256"),
      "install_path": "/usr/local/bin/ployz-ebpf-ctl"
    },
    "nats_server": {
      "version": $(json_string "$nats_version"),
      "source": $(json_string "$nats_binary"),
      "sha256": $(json_string "$NATS_SERVER_SHA256"),
      "binary": $(json_string "$nats_binary"),
      "config": $(json_string "$nats_config")
    }
  }
}
EOF
}

install -d -m 0755 "$install_dir"

# Local mode installs only the operator CLI: no root, no keeper material,
# no cluster URL, and no cluster connection attempt.
if [ "$install_mode" = "local" ]; then
  PLOYZCTL_URL="$(resolve_release_value PLOYZCTL_URL "${PLOYZCTL_URL:-}")"
  PLOYZCTL_SHA256="$(resolve_release_value PLOYZCTL_SHA256 "${PLOYZCTL_SHA256:-}")"
  download_verified "$PLOYZCTL_URL" "$PLOYZCTL_SHA256" "$tmp_file"
  install -m 0755 "$tmp_file" "$ployzctl_bin"
  echo "installed $ployzctl_bin"
  case ":${PATH}:" in
    *":${install_dir}:"*) ;;
    *)
      echo "add ${install_dir} to your PATH to run ployzctl"
      ;;
  esac
  exit 0
fi

PLOYZ_KEEPER_URL="$(resolve_release_value PLOYZ_KEEPER_URL "${PLOYZ_KEEPER_URL:-}")"
PLOYZ_KEEPER_SHA256="$(resolve_release_value PLOYZ_KEEPER_SHA256 "${PLOYZ_KEEPER_SHA256:-}")"
download_verified "$PLOYZ_KEEPER_URL" "$PLOYZ_KEEPER_SHA256" "$tmp_file"
install -d -m 0700 "$state_dir"
install -m 0755 "$tmp_file" "$keeper_bin"

if [ "$install_mode" = "first-node" ]; then
  write_first_node_spec
  "$keeper_bin" first-node-install --spec "$first_node_spec_file"
  exit 0
fi

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
