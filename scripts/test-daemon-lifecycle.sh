#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/bin" "$TMP/install" "$TMP/systemd" "$TMP/state" "$TMP/run"
LOG=$TMP/commands.log
export LOG
: > "$LOG"

cat > "$TMP/bin/docker" <<'EOF'
#!/bin/sh
echo "docker $*" >> "$LOG"
case "$*" in
    'ps -aq --filter label=ployz.managed') echo managed-container ;;
    'ps -aq --filter name=^/ployz-corrosion$') echo corrosion-container ;;
    'network ls -q --filter name=^ployz$') echo ployz-network ;;
    'rm -f '*)
        grep -Eq '^systemctl stop .*ployz-volume-plugin\.(socket|service)' "$LOG" && {
            echo "volume plugin stopped before managed containers were removed" >&2
            exit 1
        }
        [ -x "$INSTALL_BIN_DIR/ployzd" ] || {
            echo "daemon removed before managed containers were removed" >&2
            exit 1
        }
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

touch "$TMP/install/ployzd" "$TMP/install/ployz-uninstall" "$TMP/install/ployz-corrosion"
chmod 0755 "$TMP/install/ployzd" "$TMP/install/ployz-uninstall"
touch "$TMP/docker" "$TMP/images" "$TMP/volumes" "$TMP/docker-config"

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

echo "destructive daemon uninstall contract passed"
