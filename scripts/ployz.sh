#!/bin/sh
set -eu

: "${PLOYZ_KEEPER_URL:?set PLOYZ_KEEPER_URL}"
: "${PLOYZ_KEEPER_SHA256:?set PLOYZ_KEEPER_SHA256}"

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

install_dir="${PLOYZ_INSTALL_DIR:-/usr/local/bin}"
systemd_dir="${PLOYZ_SYSTEMD_DIR:-/etc/systemd/system}"
keeper_bin="${install_dir}/ployz-keeper"
keeper_unit="${systemd_dir}/ployz-keeper.service"
tmp_file="$(mktemp)"

cleanup() {
  rm -f "$tmp_file"
}
trap cleanup EXIT

curl -fsSL "$PLOYZ_KEEPER_URL" -o "$tmp_file"
printf '%s  %s\n' "$PLOYZ_KEEPER_SHA256" "$tmp_file" | sha256sum -c -

install -d -m 0755 "$install_dir" "$systemd_dir"
install -m 0755 "$tmp_file" "$keeper_bin"

cat > "$keeper_unit" <<UNIT
[Unit]
Description=Ployz Keeper
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=${keeper_bin}
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
UNIT

systemctl daemon-reload
systemctl enable --now ployz-keeper.service
