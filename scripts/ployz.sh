#!/bin/sh
set -eu

: "${PLOYZ_KEEPER_URL:?set PLOYZ_KEEPER_URL}"
: "${PLOYZ_KEEPER_SHA256:?set PLOYZ_KEEPER_SHA256}"

if [ "$#" -gt 1 ]; then
  echo "usage: PLOYZ_KEEPER_URL=... PLOYZ_KEEPER_SHA256=... sh ployz.sh [join-token]" >&2
  exit 1
fi

if [ "$#" -eq 1 ] && [ "${PLOYZ_JOIN_TOKEN:-}" ]; then
  echo "set join token as either an argument or PLOYZ_JOIN_TOKEN, not both" >&2
  exit 1
fi

if [ "$#" -eq 1 ]; then
  PLOYZ_JOIN_TOKEN="$1"
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
command -v systemctl >/dev/null

install_dir="/usr/local/bin"
systemd_dir="/etc/systemd/system"
state_dir="/var/lib/ployz/keeper"
keeper_bin="${install_dir}/ployz-keeper"
keeper_unit="${systemd_dir}/ployz-keeper.service"
join_token_file="${state_dir}/join-token"
tmp_file="$(mktemp)"

cleanup() {
  rm -f "$tmp_file"
}
trap cleanup EXIT

curl -fsSL "$PLOYZ_KEEPER_URL" -o "$tmp_file"
printf '%s  %s\n' "$PLOYZ_KEEPER_SHA256" "$tmp_file" | sha256sum -c -

install -d -m 0755 "$install_dir" "$systemd_dir"
install -d -m 0700 "$state_dir"
install -m 0755 "$tmp_file" "$keeper_bin"

keeper_args=""
if [ "${PLOYZ_JOIN_TOKEN:-}" ]; then
  umask 077
  printf '%s\n' "$PLOYZ_JOIN_TOKEN" > "$join_token_file"
  keeper_args=" --join-token-file ${join_token_file}"
fi

cat > "$keeper_unit" <<UNIT
[Unit]
Description=Ployz Keeper
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=${keeper_bin}${keeper_args}
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
UNIT

systemctl daemon-reload
systemctl enable --now ployz-keeper.service
