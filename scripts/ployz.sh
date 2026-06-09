#!/bin/sh
set -eu

: "${PLOYZ_KEEPER_URL:?set PLOYZ_KEEPER_URL}"
: "${PLOYZ_KEEPER_SHA256:?set PLOYZ_KEEPER_SHA256}"

if [ "$#" -gt 2 ]; then
  echo "usage: PLOYZ_KEEPER_URL=... PLOYZ_KEEPER_SHA256=... [PLOYZ_NATS_URL=...] sh ployz.sh [--join-token <token>]" >&2
  exit 1
fi

if [ "$#" -eq 1 ]; then
  echo "usage: PLOYZ_KEEPER_URL=... PLOYZ_KEEPER_SHA256=... [PLOYZ_NATS_URL=...] sh ployz.sh [--join-token <token>]" >&2
  exit 1
fi

if [ "$#" -eq 2 ]; then
  if [ "$1" != "--join-token" ]; then
    echo "unknown ployz installer argument: $1" >&2
    exit 1
  fi
  if [ "${PLOYZ_JOIN_TOKEN:-}" ]; then
    echo "set join token as either --join-token or PLOYZ_JOIN_TOKEN, not both" >&2
    exit 1
  fi
  PLOYZ_JOIN_TOKEN="$2"
fi

if [ "$(uname -s)" != "Linux" ]; then
  echo "ployz requires Linux" >&2
  exit 1
fi

if [ "$(id -u)" -ne 0 ]; then
  echo "ployz installer must run as root" >&2
  exit 1
fi

command -v curl >/dev/null
command -v install >/dev/null
command -v sha256sum >/dev/null

install_dir="/usr/local/bin"
state_dir="/var/lib/ployz/keeper"
keeper_bin="${install_dir}/ployz-keeper"
join_token_file="${state_dir}/join-token"
tmp_file="$(mktemp)"

cleanup() {
  rm -f "$tmp_file"
}
trap cleanup EXIT

curl -fsSL "$PLOYZ_KEEPER_URL" -o "$tmp_file"
printf '%s  %s\n' "$PLOYZ_KEEPER_SHA256" "$tmp_file" | sha256sum -c -

install -d -m 0755 "$install_dir"
install -d -m 0700 "$state_dir"
install -m 0755 "$tmp_file" "$keeper_bin"

if [ "${PLOYZ_JOIN_TOKEN:-}" ]; then
  umask 077
  printf '%s\n' "$PLOYZ_JOIN_TOKEN" > "$join_token_file"
fi

if [ "${PLOYZ_JOIN_TOKEN:-}" ]; then
  "$keeper_bin" --join-token-file "$join_token_file"
fi
