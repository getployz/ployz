#!/usr/bin/env bash
# Qualify draft musl archives against real Linux Machines.
# Pass SSH targets. This script does not pick a cloud vendor.
#
#   PLOYZ_QUALIFY_HOSTS='root@192.0.2.10 root@192.0.2.11' \
#   PLOYZ_ARTIFACT_DIR=/path/to/dist \
#   scripts/qualify-release.sh
#
# Optional: PLOYZ_QUALIFY_DRY_RUN=1, PLOYZ_QUALIFY_SSH_OPTS, PLOYZ_QUALIFY_SSH_KEY,
# PLOYZ_QUALIFY_CONTEXT, PLOYZ_QUALIFY_RESET=1.
# Hosts must be uninitialized unless PLOYZ_QUALIFY_RESET=1. Reset destroys
# managed containers on that Machine.

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
COMPOSE_DIR=$ROOT/scripts/qualify-release
HOSTS=${PLOYZ_QUALIFY_HOSTS:-}
ARTIFACT_DIR=${PLOYZ_ARTIFACT_DIR:-}
DRY_RUN=${PLOYZ_QUALIFY_DRY_RUN:-0}
SSH_OPTS=${PLOYZ_QUALIFY_SSH_OPTS:-"-o StrictHostKeyChecking=accept-new"}
CONTEXT=${PLOYZ_QUALIFY_CONTEXT:-qualify}
RESET=${PLOYZ_QUALIFY_RESET:-0}
CONFIG_DIR=

error() { echo "ERROR: $*" >&2; exit 1; }

need_archive() {
    [ -f "$ARTIFACT_DIR/$1" ] || error "PLOYZ_ARTIFACT_DIR is missing $1"
}

cli_archive() {
    case "$(uname -s):$(uname -m)" in
        Linux:x86_64) echo ployz_linux_amd64.tar.gz ;;
        Linux:aarch64) echo ployz_linux_arm64.tar.gz ;;
        Darwin:x86_64) echo ployz_macos_amd64.tar.gz ;;
        Darwin:arm64) echo ployz_macos_arm64.tar.gz ;;
        *) error "unsupported controller platform $(uname -s) $(uname -m)" ;;
    esac
}

daemon_archive() {
    case "$1" in
        x86_64) echo ployzd_linux_amd64.tar.gz ;;
        aarch64) echo ployzd_linux_arm64.tar.gz ;;
        *) error "unsupported Machine architecture $1" ;;
    esac
}

if [ -n "${PLOYZ_QUALIFY_SSH_KEY:-}" ]; then
    SSH_OPTS="$SSH_OPTS -i $PLOYZ_QUALIFY_SSH_KEY"
fi

ssh_identity() {
    # shellcheck disable=SC2086
    set -- $SSH_OPTS
    while [ $# -gt 0 ]; do
        case $1 in
            -i | --identity)
                printf '%s\n' "${2:-}"
                return
                ;;
        esac
        shift
    done
}

SSH_KEY=$(ssh_identity)

ssh_host() {
    # shellcheck disable=SC2086
    ssh $SSH_OPTS "$1" "${@:2}"
}

[ -n "$HOSTS" ] || error "set PLOYZ_QUALIFY_HOSTS to one or more user@host targets"
[ -n "$ARTIFACT_DIR" ] || error "set PLOYZ_ARTIFACT_DIR to the draft archive directory"
[ -f "$COMPOSE_DIR/compose.yaml" ] || error "missing $COMPOSE_DIR/compose.yaml"

read -r -a HOST_LIST <<<"$HOSTS"
[ "${#HOST_LIST[@]}" -ge 1 ] || error "PLOYZ_QUALIFY_HOSTS is empty"

need_archive ployz_linux_amd64.tar.gz
need_archive ployz_linux_arm64.tar.gz
need_archive ployzd_linux_amd64.tar.gz
need_archive ployzd_linux_arm64.tar.gz
need_archive "$(cli_archive)"

if [ "$DRY_RUN" != 0 ]; then
    echo "qualify dry-run"
    echo "hosts: ${HOST_LIST[*]}"
    echo "artifacts: $ARTIFACT_DIR"
    echo "compose: $COMPOSE_DIR/compose.yaml"
    echo "ssh-key: ${SSH_KEY:-cli-default}"
    if [ "$RESET" != 0 ]; then
        echo "reset: yes (--yes on machine init)"
    else
        echo "reset: no (initialized hosts fail without PLOYZ_QUALIFY_RESET=1)"
    fi
    echo "steps: install daemon from PLOYZ_RELEASE_DIR (always replace), machine init --no-install, machine add, deploy named volume qualify-data, volume ls"
    exit 0
fi

work=$(mktemp -d)
trap 'rm -rf "$work" "$CONFIG_DIR"' EXIT
tar -xzf "$ARTIFACT_DIR/$(cli_archive)" -C "$work"
PLOYZ=$work/ployz
[ -x "$PLOYZ" ] || error "CLI archive did not contain ployz"
version=$("$PLOYZ" version) || error "could not read ployz version from the artifact"
CONFIG_DIR=$(mktemp -d)
export PLOYZ_CONFIG=$CONFIG_DIR/config.yaml

install_host() {
    local host=$1 arch archive remote
    arch=$(ssh_host "$host" uname -m)
    archive=$(daemon_archive "$arch")
    remote=/tmp/ployz-qualify-$$
    ssh_host "$host" mkdir -p "$remote"
    # shellcheck disable=SC2086
    scp $SSH_OPTS "$ROOT/scripts/install.sh" "$ARTIFACT_DIR/$archive" "$host:$remote/"
    ssh_host "$host" sudo env \
        PLOYZ_RELEASE_DIR="$remote" \
        PLOYZ_VERSION="$version" \
        bash "$remote/install.sh"
}

for host in "${HOST_LIST[@]}"; do
    echo "install $host"
    install_host "$host"
done

first=${HOST_LIST[0]}
echo "machine init $first"
init_cmd=("$PLOYZ" machine init --no-install --context "$CONTEXT")
if [ "$RESET" != 0 ]; then
    init_cmd+=(--yes)
fi
if [ -n "$SSH_KEY" ]; then
    init_cmd+=(--ssh-key "$SSH_KEY")
fi
"${init_cmd[@]}" "$first"

i=1
while [ "$i" -lt "${#HOST_LIST[@]}" ]; do
    echo "machine add ${HOST_LIST[$i]}"
    add_cmd=("$PLOYZ" machine add --yes --no-install --context "$CONTEXT")
    if [ -n "$SSH_KEY" ]; then
        add_cmd+=(--ssh-key "$SSH_KEY")
    fi
    "${add_cmd[@]}" "${HOST_LIST[$i]}"
    i=$((i + 1))
done

echo "deploy named volume"
"$PLOYZ" deploy --yes --context "$CONTEXT" -f "$COMPOSE_DIR/compose.yaml"

volumes=$("$PLOYZ" volume ls --context "$CONTEXT")
printf '%s\n' "$volumes"
printf '%s\n' "$volumes" | grep -Eq 'qualify-data' || error "named volume qualify-data did not appear in volume ls"

echo "qualify-release passed on ${HOST_LIST[*]}"
