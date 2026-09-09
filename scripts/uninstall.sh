#!/usr/bin/env bash

set -euo pipefail

PLOYZ_AUTO_CONFIRM=${PLOYZ_AUTO_CONFIRM:-false}
INSTALL_BIN_DIR=${INSTALL_BIN_DIR:-/usr/local/bin}
INSTALL_SYSTEMD_DIR=${INSTALL_SYSTEMD_DIR:-/etc/systemd/system}
PLOYZ_USER=ployz
PLOYZ_DATA_DIR=${PLOYZ_DATA_DIR:-/var/lib/ployz}
PLOYZ_RUN_DIR=${PLOYZ_RUN_DIR:-/run/ployz}

log() { echo "$1"; }
error() { echo "ERROR: $1" >&2; exit 1; }

confirm() {
    [ "$PLOYZ_AUTO_CONFIRM" = true ] && return
    read -r -p "$1 [y/N] " response
    case "$response" in y|Y|yes|YES|Yes) return ;; *) return 1 ;; esac
}

safe_owned_directory() {
    case "$1" in /var/lib/ployz|/run/ployz|/tmp/*) return ;; *) return 1 ;; esac
}

stop_loaded_units() {
    local units unit rest
    units=$(systemctl list-units --all --plain --no-legend "$1") || error "Cannot inspect $1; uninstall aborted"
    while read -r unit rest; do
        [ -n "$unit" ] || continue
        systemctl stop "$unit" || error "Cannot stop $unit; uninstall aborted"
    done <<< "$units"
}

main() {
    [ "$EUID" -eq 0 ] || error "Run this uninstaller with sudo or as root"
    safe_owned_directory "$PLOYZ_DATA_DIR" || error "Refusing unsafe data directory: $PLOYZ_DATA_DIR"
    safe_owned_directory "$PLOYZ_RUN_DIR" || error "Refusing unsafe run directory: $PLOYZ_RUN_DIR"
    log "This removes Ployz state and managed containers but retains Docker, images, and named volumes."
    if ! confirm "Continue with destructive Ployz uninstall?"; then
        log "Uninstall cancelled"
        return
    fi

    stop_loaded_units 'ployz-upgrade-*.service'
    # Share MutationGate's flock so installation cannot race destructive cleanup.
    umask 077
    [ ! -L "$PLOYZ_RUN_DIR" ] || error "Refusing symlink runtime directory: $PLOYZ_RUN_DIR"
    mkdir -p "$PLOYZ_RUN_DIR"
    # Remove the service account's ability to swap the lock between inspection and open.
    chown root:root "$PLOYZ_RUN_DIR"
    chmod 0750 "$PLOYZ_RUN_DIR"
    local lock_path="$PLOYZ_RUN_DIR/.install.lock"
    if [ -L "$lock_path" ] || { [ -e "$lock_path" ] && [ ! -f "$lock_path" ]; }; then
        error "Refusing symlink or non-regular installation lock: $lock_path"
    fi
    exec {installation_lock}<>"$lock_path"
    flock -n "$installation_lock" || error "Ployz mutation or installation is active; retry uninstall"
    stop_loaded_units ployz.service
    # Catch an accepted worker launched during the first stop, now blocked on our lock.
    stop_loaded_units 'ployz-upgrade-*.service'
    if command -v docker >/dev/null 2>&1; then
        readarray -t containers < <(docker ps -aq --filter label=ployz.managed)
        if [ "${#containers[@]}" -gt 0 ]; then
            docker rm -f "${containers[@]}"
        fi
        readarray -t corrosion < <(docker ps -aq --filter name=^/ployz-corrosion$)
        if [ "${#corrosion[@]}" -gt 0 ]; then
            docker rm -f "${corrosion[@]}"
        fi
        readarray -t network < <(docker network ls -q --filter name=^ployz$)
        if [ "${#network[@]}" -gt 0 ]; then
            docker network rm "${network[@]}"
        fi
    fi

    systemctl stop ployz-volume-plugin.socket ployz-volume-plugin.service 2>/dev/null || true
    systemctl disable ployz.service ployz-volume-plugin.socket ployz-volume-plugin.service 2>/dev/null || true
    rm -f "$INSTALL_SYSTEMD_DIR/ployz.service" \
        "$INSTALL_SYSTEMD_DIR/ployz-volume-plugin.socket" \
        "$INSTALL_SYSTEMD_DIR/ployz-volume-plugin.service"
    systemctl daemon-reload
    rm -f "$INSTALL_BIN_DIR/ployzd"

    if command -v ip >/dev/null 2>&1 && ip link show ployz >/dev/null 2>&1; then
        ip link delete ployz
    fi

    rm -rf "$PLOYZ_DATA_DIR"
    # Preserve the locked inode until reboot; replacing it would bypass exclusion.
    find "$PLOYZ_RUN_DIR" -mindepth 1 -maxdepth 1 ! -name .install.lock -exec rm -rf -- {} +
    if id "$PLOYZ_USER" >/dev/null 2>&1; then userdel "$PLOYZ_USER"; fi
    if getent group "$PLOYZ_USER" >/dev/null 2>&1; then groupdel "$PLOYZ_USER"; fi
    rm -f "$INSTALL_BIN_DIR/ployz-uninstall"
    log "Ployz uninstalled; Docker, images, named volumes, and Docker configuration retained"
}

main "$@"
