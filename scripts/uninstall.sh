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

uninstall_disposition() {
    case "$1" in
        docker|docker-images|docker-volumes|docker-configuration) echo retain ;;
        *) echo remove ;;
    esac
}

confirm() {
    [ "$PLOYZ_AUTO_CONFIRM" = true ] && return
    read -r -p "$1 [y/N] " response
    case "$response" in y|Y|yes|YES|Yes) return ;; *) return 1 ;; esac
}

safe_owned_directory() {
    case "$1" in /var/lib/ployz|/run/ployz|/tmp/*) return ;; *) return 1 ;; esac
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

    systemctl stop ployz.service 2>/dev/null || true
    systemctl disable ployz.service 2>/dev/null || true
    rm -f "$INSTALL_SYSTEMD_DIR/ployz.service"
    systemctl daemon-reload

    rm -f "$INSTALL_BIN_DIR/ployzd"
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
    if command -v ip >/dev/null 2>&1 && ip link show ployz >/dev/null 2>&1; then
        ip link delete ployz
    fi

    rm -rf "$PLOYZ_DATA_DIR" "$PLOYZ_RUN_DIR"
    if id "$PLOYZ_USER" >/dev/null 2>&1; then userdel "$PLOYZ_USER"; fi
    if getent group "$PLOYZ_USER" >/dev/null 2>&1; then groupdel "$PLOYZ_USER"; fi
    rm -f "$INSTALL_BIN_DIR/ployz-uninstall"
    log "Ployz uninstalled; Docker, images, named volumes, and Docker configuration retained"
}

if [ "${PLOYZ_UNINSTALL_TEST_ONLY:-false}" != true ]; then
    main "$@"
fi
