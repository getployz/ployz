#!/usr/bin/env bash

set -euo pipefail

export LC_ALL="${LC_ALL:-C.UTF-8}"

INSTALL_ONLY=${INSTALL_ONLY:-false}
INSTALL_BIN_DIR=${INSTALL_BIN_DIR:-/usr/local/bin}
INSTALL_SYSTEMD_DIR=${INSTALL_SYSTEMD_DIR:-/etc/systemd/system}
PLOYZ_GITHUB_URL=${PLOYZ_GITHUB_URL:-https://github.com/getployz/ployz2}
PLOYZ_CHANNEL_URL=${PLOYZ_CHANNEL_URL:-https://ployz.sh}
PLOYZ_VERSION=${PLOYZ_VERSION:-latest}
PLOYZ_VERSION=${PLOYZ_VERSION#v}
PLOYZ_USER=ployz
PLOYZ_GROUP_ADD_USER=${PLOYZ_GROUP_ADD_USER:-}
PLOYZ_DATA_DIR=${PLOYZ_DATA_DIR:-/var/lib/ployz}
PLOYZ_RUN_DIR=${PLOYZ_RUN_DIR:-/run/ployz}
DOCKER_DAEMON_CONFIG_FILE=${DOCKER_DAEMON_CONFIG_FILE:-/etc/docker/daemon.json}
DAEMON_REPLACED=false
DOCKER_DAEMON_CONFIG='{
  "features": { "containerd-snapshotter": true },
  "live-restore": true,
  "log-driver": "json-file",
  "log-opts": { "max-size": "10m", "max-file": "3" }
}'

log() { echo "$1"; }
warning() { echo "WARNING: $1" >&2; }
error() { echo "ERROR: $1" >&2; exit 1; }
command_exists() { command -v "$1" >/dev/null 2>&1; }

channel_version_from_file() {
    local version
    version=$(tr -d ' \t\r\n' < "$1")
    echo "$version" | grep -Eq '^v?[0-9]+\.[0-9]+\.[0-9]+(-beta\.[0-9]+)?$' || return 1
    echo "$version"
}

resolve_install() {
    local requested=${1#v} dest resolved name
    case "$requested" in
        latest | stable | '') name=stable ;;
        beta) name=beta ;;
        *)
            printf '%s\n' "$requested"
            return 0
            ;;
    esac
    dest=$(mktemp)
    if ! curl -fsSL -o "$dest" "$PLOYZ_CHANNEL_URL/$name" || ! resolved=$(channel_version_from_file "$dest"); then
        rm -f "$dest"
        error "$name channel is unavailable"
    fi
    rm -f "$dest"
    printf '%s\n' "${resolved#v}"
}

daemon_archive() {
    case "$1" in
        x86_64) echo ployzd_linux_amd64.tar.gz ;;
        aarch64) echo ployzd_linux_arm64.tar.gz ;;
        *) return 1 ;;
    esac
}

daemon_action() {
    local installed=$1 target=$2 mode=$3
    if [ -z "$installed" ]; then
        echo replace
    elif [ "$mode" = pin ]; then
        [ "$installed" = "$target" ] && echo keep || echo replace
    elif [ "$installed" = "$target" ]; then
        echo keep
    else
        local newest
        newest=$(printf '%s\n%s\n' "${installed//-/\~}" "$target" | sort -V | tail -n1)
        [ "$newest" = "$target" ] && echo replace || echo keep
    fi
}

verify_system() {
    [ "$(uname -s)" = Linux ] || error "Ployz Machine must be Linux"
    daemon_archive "$(uname -m)" >/dev/null || error "Unsupported architecture: $(uname -m)"
    if [ ! -d /run/systemd/system ] && [ "$INSTALL_ONLY" != true ]; then
        error "Ployz requires systemd"
    fi
}

install_prerequisites() {
    command_exists curl && return
    if command_exists apt-get; then
        apt-get update -qq >/dev/null
        DEBIAN_FRONTEND=noninteractive apt-get install -y -qq curl ca-certificates
    elif command_exists dnf; then dnf install -y curl ca-certificates
    elif command_exists yum; then yum install -y curl ca-certificates
    elif command_exists pacman; then pacman -Sy --noconfirm curl ca-certificates
    elif command_exists zypper; then zypper --non-interactive install curl ca-certificates
    else error "curl is required and no supported package manager was found"
    fi
}

install_docker() {
    if command_exists dockerd; then
        if [ "$INSTALL_ONLY" != true ] && ! docker info -f '{{ .DriverStatus }}' 2>/dev/null | grep -q io.containerd.snapshotter; then
            warning "Docker is retained unchanged; enable its containerd image store for best results"
        fi
        return
    fi
    curl -fsSL https://get.docker.com | sh
    mkdir -p "$(dirname "$DOCKER_DAEMON_CONFIG_FILE")"
    printf '%s\n' "$DOCKER_DAEMON_CONFIG" > "$DOCKER_DAEMON_CONFIG_FILE"
    [ "$INSTALL_ONLY" = true ] || systemctl restart docker
}

create_user_and_directories() {
    if ! id "$PLOYZ_USER" >/dev/null 2>&1; then
        useradd --system --home-dir /nonexistent --shell /usr/sbin/nologin --user-group "$PLOYZ_USER"
    fi
    if [ -n "$PLOYZ_GROUP_ADD_USER" ]; then
        gpasswd --add "$PLOYZ_GROUP_ADD_USER" "$PLOYZ_USER" >/dev/null
    fi
    install -d -m 0750 -o "$PLOYZ_USER" -g "$PLOYZ_USER" "$PLOYZ_DATA_DIR" "$PLOYZ_RUN_DIR"
}

install_binaries() {
    local archive installed_version target action base_url tmp_dir requested mode
    archive=$(daemon_archive "$(uname -m)") || error "Unsupported architecture: $(uname -m)"
    requested=$PLOYZ_VERSION
    case "$requested" in
        latest | stable | beta | '') mode=floating ;;
        *) mode=pin ;;
    esac
    target=$(resolve_install "$requested")
    installed_version=""
    if [ -x "$INSTALL_BIN_DIR/ployzd" ]; then
        installed_version=$("$INSTALL_BIN_DIR/ployzd" version 2>/dev/null || true)
    fi

    base_url="$PLOYZ_GITHUB_URL/releases/download/v$target"

    action=$(daemon_action "$installed_version" "$target" "$mode")
    if [ "$action" = keep ]; then
        log "ployzd ${installed_version} retained"
        return
    fi

    tmp_dir=$(mktemp -d)
    # shellcheck disable=SC2064
    trap "rm -rf '$tmp_dir'" EXIT
    # TODO(artifact integrity): verify ployzd checksums or signatures if Ployz publishes them; this boundary intentionally relies on TLS alone.
    curl -fsSL -o "$tmp_dir/$archive" "$base_url/$archive" || error "Failed to download $archive"
    tar -xzf "$tmp_dir/$archive" -C "$tmp_dir"
    install -m 0755 "$tmp_dir/ployzd" "$INSTALL_BIN_DIR/ployzd"
    install -m 0755 "$tmp_dir/ployz-uninstall" "$INSTALL_BIN_DIR/ployz-uninstall"
    DAEMON_REPLACED=true
    # UT-148: Machines deliberately receive no CLI binary or alias.
}

install_systemd() {
    cat > "$INSTALL_SYSTEMD_DIR/ployz.service" <<EOF
[Unit]
Description=Ployz Machine daemon
After=network-online.target docker.service
Wants=network-online.target

[Service]
Type=notify
ExecStart=$INSTALL_BIN_DIR/ployzd
# Set PLOYZ_LOG=debug in /etc/default/ployz to raise verbosity.
EnvironmentFile=-/etc/default/ployz
TimeoutStartSec=20
Restart=always
RestartSec=2
NoNewPrivileges=true
ProtectSystem=full
ProtectControlGroups=true
ProtectHome=read-only
ProtectKernelTunables=true
PrivateTmp=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK
RestrictNamespaces=true

[Install]
WantedBy=multi-user.target
EOF
    if [ "$INSTALL_ONLY" != true ]; then
        systemctl daemon-reload
        systemctl enable ployz.service
    fi
}

main() {
    [ "$EUID" -eq 0 ] || error "Run this installer with sudo or as root"
    [ "$PLOYZ_VERSION" != nightly ] || error "nightly is not a supported release channel"
    verify_system
    install_prerequisites
    install_docker
    create_user_and_directories
    install_binaries
    if [ "$DAEMON_REPLACED" = true ]; then
        install_systemd
        [ "$INSTALL_ONLY" = true ] || systemctl restart ployz.service
    fi
    log "Ployz installed"
}

if [ "${PLOYZ_INSTALL_TEST_ONLY:-false}" != true ]; then
    main "$@"
fi
