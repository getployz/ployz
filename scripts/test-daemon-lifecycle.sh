#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/bin" "$TMP/release" "$TMP/install" "$TMP/systemd" "$TMP/state" "$TMP/run"
LOG=$TMP/commands.log
export LOG
: > "$LOG"

cat > "$TMP/ployzd" <<'EOF'
#!/bin/sh
[ "$1" = version ] && echo 1.2.3
EOF
chmod 0755 "$TMP/ployzd"
tar -czf "$TMP/release/ployzd_linux_amd64.tar.gz" \
    -C "$TMP" ployzd -C "$ROOT/scripts" --transform='s|uninstall.sh|ployz-uninstall|' uninstall.sh
printf 'v1.2.3\n' > "$TMP/release/stable"

cat > "$TMP/bin/uname" <<'EOF'
#!/bin/sh
case "$1" in -s) echo Linux ;; -m) echo x86_64 ;; esac
EOF
cat > "$TMP/bin/curl" <<'EOF'
#!/bin/bash
while [ "$#" -gt 0 ]; do
    if [ "$1" = -o ]; then output=$2; shift 2; else url=$1; shift; fi
done
src="$FAKE_RELEASE/${url##*/}"
[ -f "$src" ] || exit 1
cp "$src" "$output"
EOF
cat > "$TMP/bin/install" <<'EOF'
#!/bin/bash
args=()
while [ "$#" -gt 0 ]; do
    case "$1" in -o|-g) shift 2 ;; *) args+=("$1"); shift ;; esac
done
exec /usr/bin/install "${args[@]}"
EOF
cat > "$TMP/bin/id" <<'EOF'
#!/bin/sh
exit 0
EOF
cat > "$TMP/bin/dockerd" <<'EOF'
#!/bin/sh
exit 0
EOF
cat > "$TMP/bin/docker" <<'EOF'
#!/bin/sh
echo "docker $*" >> "$LOG"
case "$*" in
    'info -f {{ .DriverStatus }}') echo io.containerd.snapshotter ;;
    'ps -aq --filter label=ployz.managed') echo managed-container ;;
    'ps -aq --filter name=^/ployz-corrosion$') echo corrosion-container ;;
    'network ls -q --filter name=^ployz$') echo ployz-network ;;
    'rm -f '*)
        if grep -Eq '^systemctl stop .*ployz-volume-plugin\.(socket|service)' "$LOG" || [ ! -x "$INSTALL_BIN_DIR/ployzd" ]; then
            echo "Docker could not unmount after volume plugin shutdown" >&2
            exit 1
        fi
        ;;
esac
EOF
cat > "$TMP/bin/systemctl" <<'EOF'
#!/bin/sh
echo "systemctl $*" >> "$LOG"
EOF
cat > "$TMP/bin/ip" <<'EOF'
#!/bin/sh
echo "ip $*" >> "$LOG"
exit 0
EOF
for command in getent userdel groupdel; do
    cp "$TMP/bin/systemctl" "$TMP/bin/$command"
done
chmod 0755 "$TMP/bin"/*

run_installer() {
    sudo env PATH="$TMP/bin:$PATH" LOG="$LOG" FAKE_RELEASE="$TMP/release" PLOYZ_GITHUB_URL=https://example.invalid \
        PLOYZ_VERSION="$1" INSTALL_BIN_DIR="$TMP/install" INSTALL_SYSTEMD_DIR="$TMP/systemd" \
        PLOYZ_DATA_DIR="$TMP/state" PLOYZ_RUN_DIR="$TMP/run" bash "$ROOT/scripts/install.sh"
}

set_installed_version() {
    sudo tee "$TMP/install/ployzd" >/dev/null <<EOF
#!/bin/sh
[ "\$1" = version ] && echo $1
EOF
    sudo chmod 0755 "$TMP/install/ployzd"
}

touch "$TMP/state/preserved"
run_installer latest
[ -x "$TMP/install/ployzd" ]
[ -x "$TMP/install/ployz-uninstall" ]
[ -f "$TMP/state/preserved" ]
grep -Fq 'EnvironmentFile=-/etc/default/ployz' "$TMP/systemd/ployz.service"
grep -Fxq 'RestartPreventExitStatus=78' "$TMP/systemd/ployz.service"
grep -Fxq 'Before=docker.service' "$TMP/systemd/ployz-volume-plugin.socket"
grep -Fxq 'ListenStream=/run/docker/plugins/ployz.sock' "$TMP/systemd/ployz-volume-plugin.socket"
grep -Fxq 'Accept=no' "$TMP/systemd/ployz-volume-plugin.socket"
grep -Fxq 'Service=ployz-volume-plugin.service' "$TMP/systemd/ployz-volume-plugin.socket"
grep -Fxq 'Sockets=ployz-volume-plugin.socket' "$TMP/systemd/ployz-volume-plugin.service"
grep -Fq 'ExecStart='"$TMP/install"'/ployzd volume-plugin' "$TMP/systemd/ployz-volume-plugin.service"
for directive in ProtectSystem=full ProtectControlGroups=true ProtectHome=read-only ProtectKernelTunables=true PrivateTmp=true; do
    if grep -Fxq "$directive" "$TMP/systemd/ployz-volume-plugin.service"; then
        echo "Volume plugin service isolates ZFS mounts with $directive" >&2
        exit 1
    fi
done
grep -Fq 'systemctl enable --now ployz-volume-plugin.socket' "$LOG"
if grep -Fq 'systemctl enable --now ployz-volume-plugin.service' "$LOG"; then
    echo "Volume plugin service was enabled instead of socket-activated" >&2
    exit 1
fi
grep -Fq 'systemctl restart ployz.service' "$LOG"
grep -Fq 'systemctl try-restart ployz-volume-plugin.service' "$LOG"
socket_enable_line=$(grep -nF 'systemctl enable --now ployz-volume-plugin.socket' "$LOG" | head -n 1 | cut -d: -f1)
docker_check_line=$(grep -nF 'docker info -f {{ .DriverStatus }}' "$LOG" | head -n 1 | cut -d: -f1)
if [ "$socket_enable_line" -ge "$docker_check_line" ]; then
    echo "Volume plugin socket was activated after Docker installation" >&2
    exit 1
fi

: > "$LOG"
run_installer latest
if grep -Fq 'systemctl restart ployz.service' "$LOG"; then
    echo "equal latest restarted ployz.service" >&2
    exit 1
fi

sudo rm -f "$TMP/systemd/ployz.service"
: > "$LOG"
run_installer latest
grep -Fq 'systemctl restart ployz.service' "$LOG"
[ -f "$TMP/systemd/ployz.service" ]

: > "$LOG"
set_installed_version 1.2.2
run_installer latest
grep -Fq 'systemctl restart ployz.service' "$LOG"
grep -Fxq 'RestartPreventExitStatus=78' "$TMP/systemd/ployz.service"
[ -f "$TMP/state/preserved" ]

: > "$LOG"
set_installed_version 1.2.4
run_installer latest
if grep -Fq 'systemctl restart ployz.service' "$LOG"; then
    echo "newer-than-latest daemon was replaced" >&2
    exit 1
fi
[ "$("$TMP/install/ployzd" version)" = 1.2.4 ]

: > "$LOG"
run_installer 1.2.2
grep -Fq 'systemctl restart ployz.service' "$LOG"
[ "$("$TMP/install/ployzd" version)" = 1.2.3 ]
[ -f "$TMP/state/preserved" ]

touch "$TMP/docker" "$TMP/images" "$TMP/volumes" "$TMP/docker-config" "$TMP/install/ployz-corrosion"
: > "$LOG"
sudo env PATH="$TMP/bin:$PATH" LOG="$LOG" PLOYZ_AUTO_CONFIRM=true INSTALL_BIN_DIR="$TMP/install" \
    INSTALL_SYSTEMD_DIR="$TMP/systemd" PLOYZ_DATA_DIR="$TMP/state" PLOYZ_RUN_DIR="$TMP/run" \
    bash "$ROOT/scripts/uninstall.sh"
[ ! -e "$TMP/install/ployzd" ]
[ ! -e "$TMP/install/ployz-uninstall" ]
[ -e "$TMP/install/ployz-corrosion" ]
[ ! -e "$TMP/state" ]
[ ! -e "$TMP/run" ]
[ -f "$TMP/docker" ] && [ -f "$TMP/images" ] && [ -f "$TMP/volumes" ] && [ -f "$TMP/docker-config" ]
grep -Fq 'docker rm -f managed-container' "$LOG"
grep -Fq 'docker rm -f corrosion-container' "$LOG"
grep -Fq 'docker network rm ployz-network' "$LOG"
grep -Fq 'ip link delete ployz' "$LOG"

echo "daemon replacement and destructive uninstall contracts passed"
