#!/usr/bin/env bash
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=l
apt-get update -qq
apt-get install -y -qq --no-install-recommends ca-certificates curl openssh-server wireguard-tools iproute2 iputils-ping
# The actual installer prepares Docker, ZFS and systemd once, in the template.
/opt/ployz-verify/ployzd install --version "$1" --storage zfs
docker pull "$2"
docker pull nginx:1.29-alpine
docker pull caddy:2
# Enrollment discovers an exact Caddy tag; cache that tag as well as the major alias.
caddy_version=$(docker run --rm --entrypoint caddy caddy:2 version)
caddy_version=${caddy_version%% *}
docker tag caddy:2 "caddy:${caddy_version#v}"
# Releases before the Log Store ship no ployz-observe unit.
observe=
if systemctl cat ployz-observe.service >/dev/null 2>&1; then observe=ployz-observe.service; fi
systemctl stop ployz.socket ployz.service ployz-volume-plugin.socket ployz-volume-plugin.service $observe
systemctl disable ployz.socket ployz.service ployz-volume-plugin.socket $observe
systemctl enable ssh docker
# Clone dependencies and cached images, then create each Machine's identity on first start.
rm -rf /var/lib/ployz /root/.ployz /root/.ssh /opt/ployz-verify
install -d -m 750 -o ployz -g ployz /var/lib/ployz
install -d -m 700 /root/.ssh
