#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d)
trap 'sudo rm -rf "$TMP"' EXIT
mkdir -p "$TMP/bin" "$TMP/install" "$TMP/systemd" "$TMP/state" "$TMP/run"
LOG=$TMP/commands.log
SCENARIO=active
export SCENARIO
export LOG
: > "$LOG"

cat > "$TMP/bin/docker" <<'EOF'
#!/bin/sh
echo "docker $*" >> "$LOG"
[ ! -e "$PLOYZ_RUN_DIR/worker" ] && [ ! -e "$PLOYZ_RUN_DIR/daemon" ] || {
    echo "unsafe cleanup: active upgrade worker or daemon" | tee -a "$LOG" >&2
    exit 1
}
if flock -n "$PLOYZ_RUN_DIR/.install.lock" true; then
    echo "unsafe cleanup: no installation ownership" | tee -a "$LOG" >&2
    exit 1
fi
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
case "$*" in
    'list-units '*)
        [ "$SCENARIO" != list-failure ] || exit 1
        case "$*" in
            *'ployz-upgrade-*.service')
                [ ! -e "$PLOYZ_RUN_DIR/worker" ] || echo 'ployz-upgrade-test.service loaded active running Upgrade'
                ;;
            *'ployz.service')
                [ ! -e "$PLOYZ_RUN_DIR/daemon" ] || echo 'ployz.service loaded active running Daemon'
                ;;
        esac
        ;;
    'stop ployz-upgrade-test.service')
        [ "$SCENARIO" != worker-stop-failure ] || exit 1
        rm -f "$PLOYZ_RUN_DIR/worker"
        ;;
    'stop ployz.service')
        [ "$SCENARIO" != daemon-stop-failure ] || exit 1
        rm -f "$PLOYZ_RUN_DIR/daemon"
        # An accepted worker can appear after the initial worker stop.
        [ "$SCENARIO" != late ] || touch "$PLOYZ_RUN_DIR/worker"
        ;;
esac
exit 0
EOF
cat > "$TMP/bin/ip" <<'EOF'
#!/bin/sh
echo "ip $*" >> "$LOG"
exit 0
EOF
for command in getent userdel groupdel; do
    cp "$TMP/bin/ip" "$TMP/bin/$command"
done
chmod 0755 "$TMP/bin"/*

run_uninstall() {
    sudo env PATH="$TMP/bin:$PATH" LOG="$LOG" SCENARIO="$SCENARIO" PLOYZ_AUTO_CONFIRM=true \
        INSTALL_BIN_DIR="$TMP/install" INSTALL_SYSTEMD_DIR="$TMP/systemd" \
        PLOYZ_DATA_DIR="$TMP/state" PLOYZ_RUN_DIR="$TMP/run" bash "$ROOT/scripts/uninstall.sh"
}

for SCENARIO in symlink-failure dangling-failure fifo-failure directory-failure active late absent worker-stop-failure list-failure daemon-stop-failure busy; do
    : > "$LOG"
    mkdir -p "$TMP/state" "$TMP/run"
    touch "$TMP/state/receipt" "$TMP/run/socket"
    if [ "$SCENARIO" != absent ]; then touch "$TMP/run/worker" "$TMP/run/daemon"; fi
    touch "$TMP/install/ployzd" "$TMP/install/ployzd-tailcat" "$TMP/install/ployz-uninstall" "$TMP/install/ployz-corrosion"
    printf 'CLI retained\n' > "$TMP/install/ployz"
    printf 'CLI helper retained\n' > "$TMP/install/ployz-tailcat"
    chmod 0755 "$TMP/install/ployzd" "$TMP/install/ployz-uninstall"
    touch "$TMP/docker" "$TMP/images" "$TMP/volumes" "$TMP/docker-config"

    if [ "$SCENARIO" = symlink-failure ]; then
        printf 'protected target\n' | sudo tee "$TMP/protected-target" >/dev/null
        sudo chmod 0600 "$TMP/protected-target"
        ln -s "$TMP/protected-target" "$TMP/run/.install.lock"
    fi
    case "$SCENARIO" in
        dangling-failure) ln -s "$TMP/missing-target" "$TMP/run/.install.lock" ;;
        fifo-failure) mkfifo "$TMP/run/.install.lock" ;;
        directory-failure) mkdir "$TMP/run/.install.lock" ;;
        active) printf 'existing lock\n' > "$TMP/run/.install.lock" ;;
    esac
    if [ "$SCENARIO" = busy ]; then
        exec {lock_fd}>"$TMP/run/.install.lock"
        flock -n "$lock_fd"
    fi
    result=0
    run_uninstall || result=$?
    case "$SCENARIO" in
        worker-stop-failure|list-failure) ;;
        *) [ "$(sudo stat -c %u "$TMP/run")" = 0 ] || {
            echo "runtime directory is not root-controlled" >&2
            exit 1
        } ;;
    esac
    # Restore test access only after uninstall has exited.
    sudo chown "$(id -u):$(id -g)" "$TMP/run"
    [ ! -e "$TMP/missing-target" ]
    if [ "$SCENARIO" = active ]; then
        [ "$(sudo cat "$TMP/run/.install.lock")" = 'existing lock' ]
    fi
    if [ "$SCENARIO" = symlink-failure ]; then
        [ "$(sudo cat "$TMP/protected-target")" = 'protected target' ] || {
            echo "uninstall truncated the symlink target" >&2
            exit 1
        }
    fi
    if [ "$result" -eq 0 ]; then
        case "$SCENARIO" in *failure|busy) echo "unexpected uninstall success: $SCENARIO" >&2; exit 1 ;; esac
        [ ! -e "$TMP/install/ployzd" ]
        [ ! -e "$TMP/install/ployzd-tailcat" ]
        [ ! -e "$TMP/install/ployz-uninstall" ]
        [ ! -e "$TMP/state" ]
        # Keep the lock inode: unlinking it lets a concurrent installer bypass ownership.
        [ "$(ls -A "$TMP/run")" = .install.lock ]
        sudo flock -n "$TMP/run/.install.lock" true
        grep -Fq 'docker rm -f managed-container' "$LOG"
        grep -Fq 'docker rm -f corrosion-container' "$LOG"
        grep -Fq 'docker network rm ployz-network' "$LOG"
        grep -Fq 'ip link delete ployz' "$LOG"
    else
        case "$SCENARIO" in *failure|busy) ;; *) echo "unexpected uninstall failure: $SCENARIO" >&2; exit 1 ;; esac
        [ -e "$TMP/install/ployzd" ] && [ -e "$TMP/install/ployz-uninstall" ]
        [ -e "$TMP/install/ployzd-tailcat" ]
        [ -e "$TMP/state/receipt" ] && [ -e "$TMP/run/socket" ]
        if grep -q '^docker ' "$LOG"; then exit 1; fi
    fi
    if grep -q '^unsafe cleanup:' "$LOG"; then exit 1; fi
    if [ "$SCENARIO" = busy ]; then exec {lock_fd}>&-; fi
    [ "$(cat "$TMP/install/ployz")" = 'CLI retained' ]
    [ "$(cat "$TMP/install/ployz-tailcat")" = 'CLI helper retained' ]
    [ -e "$TMP/install/ployz-corrosion" ]
    [ -f "$TMP/docker" ] && [ -f "$TMP/images" ] && [ -f "$TMP/volumes" ] && [ -f "$TMP/docker-config" ]
    sudo rm -rf "$TMP/run" "$TMP/state"
done

echo "destructive daemon uninstall contract passed"
