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
PLOYZ_STORAGE=${PLOYZ_STORAGE:-none}
DOCKER_DAEMON_CONFIG_FILE=${DOCKER_DAEMON_CONFIG_FILE:-/etc/docker/daemon.json}
APT_LOCK_TIMEOUT_SECONDS=300
PLOYZ_APT_CONFIG=
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

configure_apt_lock_wait() {
    local inherited_apt_config=${APT_CONFIG:-}
    PLOYZ_APT_CONFIG=$(mktemp)
    if [ -n "$inherited_apt_config" ]; then
        cat "$inherited_apt_config" > "$PLOYZ_APT_CONFIG"
        printf '\n' >> "$PLOYZ_APT_CONFIG"
    fi
    printf 'DPkg::Lock::Timeout "%s";\n' "$APT_LOCK_TIMEOUT_SECONDS" >> "$PLOYZ_APT_CONFIG"
    export APT_CONFIG="$PLOYZ_APT_CONFIG"
}

run_with_apt_lock_wait() {
    local deadline=0 error_dir error_log status tee_pid
    while true; do
        error_dir=$(mktemp -d)
        error_log=$error_dir/stderr
        mkfifo "$error_dir/pipe"
        tee "$error_log" < "$error_dir/pipe" >&2 &
        tee_pid=$!
        if LC_ALL=C "$@" 2> "$error_dir/pipe"; then
            status=0
        else
            status=$?
        fi
        wait "$tee_pid"
        if [ "$status" -eq 0 ]; then
            rm -rf "$error_dir"
            return 0
        fi
        if grep -Eq '^E: Could not get lock .*/(lists|archives)/lock\. It is held by process' "$error_log"; then
            [ "$deadline" -ne 0 ] || deadline=$((SECONDS + APT_LOCK_TIMEOUT_SECONDS))
            if [ "$SECONDS" -lt "$deadline" ]; then
                rm -rf "$error_dir"
                sleep 1
                continue
            fi
        fi
        break
    done
    if grep -Eq '^E: (Could not get lock|Unable to acquire .* lock)' "$error_log"; then
        rm -rf "$error_dir"
        error "The package-manager lock named above stayed busy for five minutes. Let its owner finish, then retry the installer."
    fi
    rm -rf "$error_dir"
    return "$status"
}

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
        run_with_apt_lock_wait apt-get update -qq >/dev/null
        run_with_apt_lock_wait env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq curl ca-certificates
    elif command_exists dnf; then dnf install -y curl ca-certificates
    elif command_exists yum; then yum install -y curl ca-certificates
    elif command_exists pacman; then pacman -Sy --noconfirm curl ca-certificates
    elif command_exists zypper; then zypper --non-interactive install curl ca-certificates
    else error "curl is required and no supported package manager was found"
    fi
}

operating_system_id() {
    local id
    [ -r /etc/os-release ] || return 1
    id=$(sed -n 's/^ID=//p' /etc/os-release | head -n1)
    id=${id#\"}
    id=${id%\"}
    [ -n "$id" ] || return 1
    printf '%s\n' "$id"
}

container_virtualization() {
    systemd-detect-virt --container 2>/dev/null || true
}

lxc_is_unprivileged() {
    local inside outside _length
    read -r inside outside _length < /proc/self/uid_map || return 1
    [ "$inside" = 0 ] && [ "$outside" != 0 ]
}

zfs_smoke_bytes() {
    echo 134217728
}

require_host_root_reserve() {
    local allocation=$1 root_size='' root_available='' reserve
    read -r root_size root_available < <(df -B1 --output=size,avail / | tail -n1) || \
        error "Could not inspect host-root capacity for ZFS storage preparation"
    case "$root_size:$root_available" in
        *[!0-9:]* | :* | *:) error "Could not read host-root capacity for ZFS storage preparation" ;;
    esac
    reserve=$((root_size / 4))
    [ "$reserve" -ge 10737418240 ] || reserve=10737418240
    if [ "$root_available" -lt $((reserve + allocation)) ]; then
        error "Host root has ${root_available} bytes available; ZFS validation needs ${allocation} bytes while preserving the ${reserve}-byte host-root reserve"
    fi
}

package_file_list_has_zfs_module() {
    local kernel=$1
    grep -F "/lib/modules/$kernel/" | grep -E '/zfs\.ko(\.[^/]*)?$' >/dev/null
}

ubuntu_zfs_module_package() {
    local kernel=$1 package package_dir archive download_failure='' archive_failure=''
    for package in \
        "linux-main-modules-zfs-$kernel" \
        "linux-modules-zfs-$kernel" \
        "linux-modules-$kernel" \
        "linux-modules-extra-$kernel"; do
        apt-cache show "$package" >/dev/null 2>&1 || continue
        if dpkg-query -L "$package" 2>/dev/null | package_file_list_has_zfs_module "$kernel"; then
            printf '%s\n' "$package"
            return
        fi
        package_dir=$(mktemp -d)
        if (cd "$package_dir" && run_with_apt_lock_wait apt-get download "$package" >/dev/null); then
            archive=$(find "$package_dir" -maxdepth 1 -type f -name '*.deb' -print -quit)
            if [ -z "$archive" ] || ! dpkg-deb -c "$archive" > "$package_dir/files"; then
                archive_failure=$package
            elif package_file_list_has_zfs_module "$kernel" < "$package_dir/files"; then
                rm -rf "$package_dir"
                printf '%s\n' "$package"
                return
            fi
        else
            download_failure=$package
        fi
        rm -rf "$package_dir"
    done
    if [ -n "$archive_failure" ]; then
        error "Could not inspect downloaded running-kernel module package $archive_failure for ZFS support"
    fi
    if [ -n "$download_failure" ]; then
        error "Could not download running-kernel module package $download_failure to inspect it for ZFS support"
    fi
    error "Ubuntu has no packaged ZFS module for the running kernel $kernel; install a supported Ubuntu kernel and retry"
}

install_zfs_packages_ubuntu() {
    local kernel=$1 module_package
    if ! run_with_apt_lock_wait apt-get update -qq >/dev/null; then
        error "Could not refresh Ubuntu packages needed for ZFS"
    fi
    if ! module_package=$(ubuntu_zfs_module_package "$kernel"); then
        return 1
    fi
    if ! run_with_apt_lock_wait env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends zfsutils-linux "$module_package"; then
        error "Could not install zfsutils-linux and running-kernel module package $module_package"
    fi
    if ! dpkg-query -L "$module_package" | package_file_list_has_zfs_module "$kernel"; then
        error "Installed package $module_package does not supply the ZFS module for running kernel $kernel"
    fi
}

zfs_arc_max_for_memory_kib() {
    local memory_kib=$1 cap
    cap=$((memory_kib * 1024 / 4))
    [ "$cap" -ge 268435456 ] || cap=268435456
    [ "$cap" -le 1073741824 ] || cap=1073741824
    printf '%s\n' "$cap"
}

zfs_arc_max() {
    local memory_kib
    memory_kib=$(awk '$1 == "MemTotal:" { print $2; exit }' /proc/meminfo)
    case "$memory_kib" in '' | *[!0-9]*) error "Could not read total RAM for the ZFS ARC limit" ;; esac
    zfs_arc_max_for_memory_kib "$memory_kib"
}

persist_zfs_arc_max() {
    local cap=$1 config
    config=$(mktemp)
    printf 'options zfs zfs_arc_max=%s\n' "$cap" > "$config"
    if ! install -D -m 0644 "$config" /etc/modprobe.d/ployz-zfs.conf; then
        rm -f "$config"
        error "Could not persist the ZFS ARC limit in /etc/modprobe.d/ployz-zfs.conf"
    fi
    rm -f "$config"
}

set_and_verify_zfs_arc_max() {
    local cap=$1 parameter=/sys/module/zfs/parameters/zfs_arc_max observed
    [ -w "$parameter" ] || error "Loaded ZFS module does not expose a writable zfs_arc_max parameter"
    printf '%s\n' "$cap" > "$parameter" || error "Could not apply zfs_arc_max=$cap to the loaded ZFS module"
    observed=$(cat "$parameter") || error "Could not read zfs_arc_max from the loaded ZFS module"
    [ "$observed" = "$cap" ] || \
        error "Loaded ZFS module reports zfs_arc_max=$observed instead of the required $cap"
}

cleanup_zfs_smoke() {
    local pool=$1 backing=$2 pool_created=$3 failure=$4
    local pool_names dataset_names mount_sources
    local cleanup_failure=''

    if [ "$pool_created" = true ]; then
        if ! zpool destroy -f "$pool"; then
            cleanup_failure="Could not destroy temporary ZFS smoke Pool $pool; destroy it, then remove $backing"
        fi
    elif ! pool_names=$(zpool list -H -o name 2>&1); then
        cleanup_failure="Could not verify whether temporary ZFS smoke Pool $pool remains; inspect and destroy it, then remove $backing: ${pool_names:-zpool returned no diagnostic output}"
    elif printf '%s\n' "$pool_names" | grep -Fxq "$pool" && ! zpool destroy -f "$pool"; then
        cleanup_failure="Could not destroy temporary ZFS smoke Pool $pool; destroy it, then remove $backing"
    fi
    if [ -z "$cleanup_failure" ]; then
        if ! pool_names=$(zpool list -H -o name 2>&1); then
            cleanup_failure="Could not verify cleanup of temporary ZFS smoke Pool $pool; inspect and destroy it, then remove $backing: ${pool_names:-zpool returned no diagnostic output}"
        elif printf '%s\n' "$pool_names" | grep -Fxq "$pool"; then
            cleanup_failure="Temporary ZFS smoke Pool $pool remains after destroy; destroy it, then remove $backing"
        elif ! dataset_names=$(zfs list -H -o name 2>&1); then
            cleanup_failure="Could not verify cleanup of temporary ZFS smoke dataset $pool; inspect and destroy its Pool, then remove $backing: ${dataset_names:-zfs returned no diagnostic output}"
        elif printf '%s\n' "$dataset_names" | awk -v pool="$pool" \
            '$0 == pool || index($0, pool "/") == 1 { found = 1 } END { exit !found }'; then
            cleanup_failure="Temporary ZFS smoke dataset $pool remains after destroy; destroy its Pool, then remove $backing"
        elif ! mount_sources=$(awk '
            { for (i = 1; i <= NF; i++) if ($i == "-" && $(i + 1) == "zfs") print $(i + 2) }
        ' /proc/self/mountinfo 2>&1); then
            cleanup_failure="Could not verify cleanup of temporary ZFS smoke mounts for $pool; inspect and unmount them, then remove $backing: ${mount_sources:-mount inspection returned no diagnostic output}"
        elif printf '%s\n' "$mount_sources" | awk -v pool="$pool" \
            '$0 == pool || index($0, pool "/") == 1 { found = 1 } END { exit !found }'; then
            cleanup_failure="Temporary ZFS smoke mount for $pool remains after destroy; unmount it, destroy the Pool, then remove $backing"
        elif ! rm -f "$backing"; then
            cleanup_failure="Could not remove temporary ZFS smoke backing file $backing"
        fi
    fi
    if [ -z "$cleanup_failure" ] && [ -e "$backing" ]; then
        cleanup_failure="Temporary ZFS smoke backing file $backing remains after cleanup"
    fi
    if [ -n "$cleanup_failure" ]; then
        [ -z "$failure" ] || cleanup_failure="$cleanup_failure (after: $failure)"
        error "$cleanup_failure"
    fi
    [ -z "$failure" ] || error "$failure"
}

restore_zfs_smoke_trap() {
    local saved=$1 signal=$2
    if [ -n "$saved" ]; then
        eval "$saved"
    else
        trap - "$signal"
    fi
}

interrupt_zfs_smoke() {
    local signal=$1 pool=$2 backing=$3 pool_created=$4
    local previous_int_trap=$5 previous_term_trap=$6
    trap - INT TERM
    [ -z "$backing" ] || cleanup_zfs_smoke "$pool" "$backing" "$pool_created" ''
    restore_zfs_smoke_trap "$previous_int_trap" INT
    restore_zfs_smoke_trap "$previous_term_trap" TERM
    kill -s "$signal" "${BASHPID:-$$}"
}

validate_zfs() {
    local bytes backing='' pool blocks block_size allocated mount_target
    local previous_int_trap previous_term_trap
    local pool_created=false failure=''
    bytes=$(zfs_smoke_bytes)
    require_host_root_reserve "$bytes"
    pool="ployz-smoke-${BASHPID:-$$}-${RANDOM}"
    previous_int_trap=$(trap -p INT)
    previous_term_trap=$(trap -p TERM)
    trap 'interrupt_zfs_smoke INT "$pool" "$backing" "$pool_created" "$previous_int_trap" "$previous_term_trap"' INT
    trap 'interrupt_zfs_smoke TERM "$pool" "$backing" "$pool_created" "$previous_int_trap" "$previous_term_trap"' TERM
    if ! backing=$(mktemp /var/tmp/ployz-zfs-smoke.XXXXXX); then
        error "Could not create a temporary host-root backing file for ZFS validation"
    fi

    if ! fallocate -l "$bytes" "$backing"; then
        failure="Could not preallocate the non-sparse ZFS smoke backing file $backing"
    elif ! read -r blocks block_size < <(stat -c '%b %B' "$backing"); then
        failure="Could not verify allocation of ZFS smoke backing file $backing"
    else
        allocated=$((blocks * block_size))
        [ "$allocated" -ge "$bytes" ] || \
            failure="ZFS smoke backing file $backing is sparse: $allocated of $bytes bytes are allocated"
    fi
    if [ -z "$failure" ]; then
        mount_target=$(findmnt -n -o TARGET -T "$backing" 2>/dev/null || true)
        [ "$mount_target" = / ] || \
            failure="ZFS smoke backing file $backing is on $mount_target instead of the host root filesystem"
    fi
    if [ -z "$failure" ]; then
        if zpool create -f -m none -o cachefile=none "$pool" "$backing"; then
            pool_created=true
        else
            failure="Could not create temporary ZFS smoke Pool $pool on $backing"
        fi
    fi
    if [ -z "$failure" ] && ! zpool list -Hp -o name,size,alloc,free "$pool" >/dev/null; then
        failure="Could not query temporary ZFS smoke Pool $pool"
    fi
    if [ -z "$failure" ] && ! zfs list -Hp -o name,mountpoint "$pool" >/dev/null; then
        failure="Could not query temporary ZFS smoke dataset $pool"
    fi

    cleanup_zfs_smoke "$pool" "$backing" "$pool_created" "$failure"
    restore_zfs_smoke_trap "$previous_int_trap" INT
    restore_zfs_smoke_trap "$previous_term_trap" TERM
}

prepare_zfs() {
    local os_id container kernel cap
    os_id=$(operating_system_id) || error "Could not identify the Linux distribution for ZFS storage preparation"
    [ "$os_id" = ubuntu ] || error "ZFS storage preparation is not supported on $os_id yet; use a supported Ubuntu release"
    container=$(container_virtualization)
    if [ "$container" = openvz ] || { [ -d /proc/vz ] && [ ! -d /proc/bc ]; }; then
        error "OpenVZ does not allow this Machine to load the host ZFS kernel module"
    fi
    if [ "$container" = lxc ] && lxc_is_unprivileged; then
        error "Unprivileged LXC does not allow this Machine to load the host ZFS kernel module"
    fi
    command_exists apt-get || error "Ubuntu apt-get is required for ZFS storage preparation"
    kernel=$(uname -r)
    require_host_root_reserve "$(zfs_smoke_bytes)"
    cap=$(zfs_arc_max)
    persist_zfs_arc_max "$cap"
    install_zfs_packages_ubuntu "$kernel"
    modprobe zfs || error "modprobe zfs failed for running kernel $kernel; verify kernel module support and container privileges"
    set_and_verify_zfs_arc_max "$cap"
    validate_zfs
    log "ZFS storage preparation validated; no Machine Pool was created"
}

prepare_storage() {
    case "$PLOYZ_STORAGE" in
        none) return ;;
        zfs) prepare_zfs ;;
        *) error "Unsupported storage preparation '$PLOYZ_STORAGE'; expected none or zfs" ;;
    esac
}

install_docker() {
    if command_exists dockerd; then
        if [ "$INSTALL_ONLY" != true ] && ! docker info -f '{{ .DriverStatus }}' 2>/dev/null | grep -q io.containerd.snapshotter; then
            warning "Docker is retained unchanged; enable its containerd image store for best results"
        fi
        return
    fi
    run_with_apt_lock_wait bash -o pipefail -c 'curl -fsSL https://get.docker.com | sh'
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
RestartPreventExitStatus=78
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
    cat > "$INSTALL_SYSTEMD_DIR/ployz-volume-plugin.socket" <<EOF
[Unit]
Description=Ployz Docker Volume plugin socket
Before=docker.service

[Socket]
ListenStream=/run/docker/plugins/ployz.sock
SocketMode=0660
DirectoryMode=0755
Accept=no
Service=ployz-volume-plugin.service

[Install]
WantedBy=sockets.target
EOF
    cat > "$INSTALL_SYSTEMD_DIR/ployz-volume-plugin.service" <<EOF
[Unit]
Description=Ployz Docker Volume plugin
Before=docker.service
After=zfs-import.target zfs-mount.service ployz-volume-plugin.socket
Requires=ployz-volume-plugin.socket docker.service

[Service]
Type=simple
ExecStart=$INSTALL_BIN_DIR/ployzd volume-plugin
Sockets=ployz-volume-plugin.socket
EnvironmentFile=-/etc/default/ployz
Restart=on-failure
RestartSec=2
NoNewPrivileges=true
RestrictAddressFamilies=AF_UNIX
RestrictNamespaces=true
EOF
    if [ "$INSTALL_ONLY" != true ]; then
        systemctl daemon-reload
        systemctl enable ployz.service
        systemctl enable --now ployz-volume-plugin.socket
    fi
}

main() {
    [ "$EUID" -eq 0 ] || error "Run this installer with sudo or as root"
    [ "$PLOYZ_VERSION" != nightly ] || error "nightly is not a supported release channel"
    verify_system
    if command_exists apt-get; then
        configure_apt_lock_wait
        trap 'rm -f "$PLOYZ_APT_CONFIG"' EXIT
    fi
    prepare_storage
    install_prerequisites
    create_user_and_directories
    unit=$INSTALL_SYSTEMD_DIR/ployz.service
    [ -f "$unit" ] || DAEMON_REPLACED=true
    install_binaries
    install_systemd
    install_docker
    if [ -n "$PLOYZ_APT_CONFIG" ]; then
        rm -f "$PLOYZ_APT_CONFIG"
        trap - EXIT
    fi
    if [ "$INSTALL_ONLY" != true ] && [ "$DAEMON_REPLACED" = true ]; then
        systemctl restart ployz.service
        systemctl try-restart ployz-volume-plugin.service
    fi
    log "Ployz installed"
}

if [ "${PLOYZ_INSTALL_TEST_ONLY:-false}" != true ]; then
    main "$@"
fi
