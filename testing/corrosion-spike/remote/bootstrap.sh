#!/usr/bin/env bash
# Usage:
#   sudo ./bootstrap.sh install <corrosion-binary> <sha256> <config-values.json> <schema-directory>
#   sudo ./bootstrap.sh clock-gate
#   sudo ./bootstrap.sh start
#
# install expects config.toml.tpl and corrosion.service beside this script.
# Run install before mesh.sh, then clock-gate and start after wg0 is active.
# config-values.json must be root-owned, mode 0600, and have this shape:
# {
#   "db_path": "/var/lib/corrosion/corrosion.db",
#   "subscriptions_path": "/var/lib/corrosion/subscriptions",
#   "schema_path": "/etc/corrosion/schema",
#   "gossip_addr": "10.77.0.1:8787",
#   "bootstrap_peers": ["10.77.0.2:8787", "10.77.0.3:8787"],
#   "api_addr": "127.0.0.1:8080",
#   "bearer_token": "...",
#   "admin_socket": "/run/corrosion/admin.sock",
#   "test_reaper": {
#     "check_interval": 10,
#     "tables": {"never_reused_rows": "30s"}
#   }
# }
# test_reaper is optional and is valid only for tables whose ids are never reused.

set -euo pipefail

readonly CORROSION_RELEASE="v1.0.0"
readonly CORROSION_EMBEDDED_VERSION="corrosion 0.2.0-beta.0"
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
readonly SCRIPT_DIR
readonly CONFIG_TEMPLATE="${SCRIPT_DIR}/config.toml.tpl"
readonly SERVICE_SOURCE="${SCRIPT_DIR}/corrosion.service"

fail() {
    printf 'bootstrap: %s\n' "$*" >&2
    exit 1
}

require_root() {
    [[ ${EUID} -eq 0 ]] || fail "run as root"
}

clock_gate() {
    require_root
    if chronyc waitsync 30 0.1 0.0 1 >/dev/null; then
        printf 'clock_sync=ready\n'
        return 0
    fi

    printf 'clock_sync=not-ready\n' >&2
    return 1
}

start_corrosion() {
    require_root
    systemctl is-active --quiet wg-quick@wg0.service ||
        fail "wg-quick@wg0.service must be active before Corrosion starts"
    systemctl start corrosion.service
    systemctl is-active --quiet corrosion.service ||
        fail "corrosion.service did not become active"
    printf 'corrosion=ready\n'
}

require_root_secret_file() {
    local path=$1

    [[ -f ${path} ]] || fail "missing file: ${path}"
    [[ $(stat -c '%u' -- "${path}") -eq 0 ]] ||
        fail "file must be owned by root: ${path}"
    if find "${path}" -prune -perm /077 -print -quit | grep -q .; then
        fail "file must not be accessible by group or other: ${path}"
    fi
}

install_corrosion() {
    [[ $# -eq 4 ]] ||
        fail "install requires <corrosion-binary> <sha256> <config-values.json> <schema-directory>"

    local binary_path=$1
    local expected_sha256=$2
    local values_path=$3
    local schema_source=$4
    local actual_sha256
    local admin_socket
    local db_path
    local installed_version
    local rendered_config
    local schema_count=0
    local schema_path
    local subscriptions_path

    require_root
    [[ -f ${binary_path} ]] || fail "missing Corrosion binary: ${binary_path}"
    [[ ${expected_sha256} =~ ^[[:xdigit:]]{64}$ ]] ||
        fail "expected SHA-256 must be exactly 64 hexadecimal characters"
    require_root_secret_file "${values_path}"
    [[ -d ${schema_source} ]] || fail "missing schema directory: ${schema_source}"
    [[ -f ${CONFIG_TEMPLATE} ]] || fail "missing template: ${CONFIG_TEMPLATE}"
    [[ -f ${SERVICE_SOURCE} ]] || fail "missing service unit: ${SERVICE_SOURCE}"

    actual_sha256=$(sha256sum -- "${binary_path}")
    actual_sha256=${actual_sha256%% *}
    [[ ${actual_sha256,,} == "${expected_sha256,,}" ]] ||
        fail "Corrosion binary SHA-256 mismatch"

    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y --no-install-recommends \
        ca-certificates \
        chrony \
        curl \
        jq \
        python3 \
        sqlite3 \
        wireguard-tools

    if ! getent group corrosion >/dev/null; then
        groupadd --system corrosion
    fi
    if ! id corrosion >/dev/null 2>&1; then
        useradd \
            --system \
            --gid corrosion \
            --home-dir /var/lib/corrosion \
            --shell /usr/sbin/nologin \
            corrosion
    fi

    install -o root -g root -m 0755 -- "${binary_path}" /usr/local/bin/corrosion
    installed_version=$(/usr/local/bin/corrosion --version)
    [[ ${installed_version} == "${CORROSION_EMBEDDED_VERSION}" ]] ||
        fail "binary does not carry the version string embedded in the official ${CORROSION_RELEASE} asset"

    install -d -o root -g corrosion -m 0750 /etc/corrosion
    rendered_config=$(mktemp /etc/corrosion/config.toml.XXXXXX)
    trap 'rm -f -- "${rendered_config:-}"' EXIT
    python3 - "${CONFIG_TEMPLATE}" "${values_path}" "${rendered_config}" <<'PY'
import json
import ipaddress
import pathlib
import re
import sys

template_path, values_path, output_path = map(pathlib.Path, sys.argv[1:])
values = json.loads(values_path.read_text())

required = {
    "db_path",
    "subscriptions_path",
    "schema_path",
    "gossip_addr",
    "bootstrap_peers",
    "api_addr",
    "bearer_token",
    "admin_socket",
}
missing = sorted(required - values.keys())
if missing:
    raise SystemExit(f"missing config values: {', '.join(missing)}")

for key in required - {"bootstrap_peers"}:
    if not isinstance(values[key], str) or not values[key]:
        raise SystemExit(f"{key} must be a non-empty string")

if not isinstance(values["bootstrap_peers"], list):
    raise SystemExit("bootstrap_peers must be an array")
if not all(isinstance(peer, str) and peer for peer in values["bootstrap_peers"]):
    raise SystemExit("bootstrap_peers entries must be non-empty strings")

path_prefixes = {
    "db_path": "/var/lib/corrosion/",
    "subscriptions_path": "/var/lib/corrosion/",
    "schema_path": "/etc/corrosion/",
    "admin_socket": "/run/corrosion/",
}
for key, prefix in path_prefixes.items():
    path = pathlib.PurePosixPath(values[key])
    if ".." in path.parts or not values[key].startswith(prefix):
        raise SystemExit(f"{key} must stay below {prefix}")

if pathlib.PurePosixPath(values["admin_socket"]).parent != pathlib.PurePosixPath("/run/corrosion"):
    raise SystemExit("admin_socket must be directly below /run/corrosion")

def parse_socket(value):
    if value.startswith("["):
        host, separator, port = value[1:].partition("]:")
    else:
        host, separator, port = value.rpartition(":")
    if not separator or not port.isascii() or not port.isdecimal():
        raise ValueError
    parsed_port = int(port)
    if not 1 <= parsed_port <= 65535:
        raise ValueError
    return ipaddress.ip_address(host), parsed_port

try:
    api_ip, _ = parse_socket(values["api_addr"])
except ValueError:
    raise SystemExit("api_addr must be a numeric socket address") from None
if not api_ip.is_loopback:
    raise SystemExit("api_addr must be a numeric loopback socket address")

try:
    gossip_ip, _ = parse_socket(values["gossip_addr"])
except ValueError:
    raise SystemExit("gossip_addr must be a numeric socket address") from None
if (
    not gossip_ip.is_private
    or gossip_ip.is_loopback
    or gossip_ip.is_link_local
    or gossip_ip.is_multicast
    or gossip_ip.is_unspecified
):
    raise SystemExit("gossip_addr must bind a private WireGuard address")

reaper_config = ""
test_reaper = values.get("test_reaper")
if test_reaper is not None:
    if not isinstance(test_reaper, dict):
        raise SystemExit("test_reaper must be an object")
    check_interval = test_reaper.get("check_interval")
    tables = test_reaper.get("tables")
    if not isinstance(check_interval, int) or isinstance(check_interval, bool) or check_interval < 1:
        raise SystemExit("test_reaper.check_interval must be a positive integer")
    if not isinstance(tables, dict) or not tables:
        raise SystemExit("test_reaper.tables must be a non-empty object")

    lines = ["[reaper]", f"check_interval = {check_interval}"]
    for table, retention in sorted(tables.items()):
        if not isinstance(table, str) or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", table):
            raise SystemExit("test_reaper table names must be SQL identifiers")
        if not isinstance(retention, str) or not re.fullmatch(r"[1-9][0-9]*[smhdwy]", retention):
            raise SystemExit("test_reaper retention must be a positive duration such as 30s or 7d")
        lines.extend(
            [
                "",
                f"[reaper.tables.{table}]",
                f"retention = {json.dumps(retention)}",
            ]
        )
    reaper_config = "\n".join(lines)

replacements = {
    "@@DB_PATH@@": json.dumps(values["db_path"]),
    "@@SUBSCRIPTIONS_PATH@@": json.dumps(values["subscriptions_path"]),
    "@@SCHEMA_PATH@@": json.dumps(values["schema_path"]),
    "@@GOSSIP_ADDR@@": json.dumps(values["gossip_addr"]),
    "@@BOOTSTRAP_PEERS@@": json.dumps(values["bootstrap_peers"]),
    "@@API_ADDR@@": json.dumps(values["api_addr"]),
    "@@BEARER_TOKEN@@": json.dumps(values["bearer_token"]),
    "@@ADMIN_SOCKET@@": json.dumps(values["admin_socket"]),
    "@@TEST_REAPER_CONFIG@@": reaper_config,
}

rendered = template_path.read_text()
for marker, replacement in replacements.items():
    rendered = rendered.replace(marker, replacement)
if "@@" in rendered:
    raise SystemExit("unresolved config template marker")
output_path.write_text(rendered)
PY
    db_path=$(jq -er '.db_path' "${values_path}")
    subscriptions_path=$(jq -er '.subscriptions_path' "${values_path}")
    schema_path=$(jq -er '.schema_path' "${values_path}")
    admin_socket=$(jq -er '.admin_socket' "${values_path}")

    install -d -o corrosion -g corrosion -m 0750 \
        /var/lib/corrosion \
        "$(dirname -- "${db_path}")" \
        "${subscriptions_path}"
    install -d -o root -g corrosion -m 0750 "${schema_path}"
    install -d -o corrosion -g corrosion -m 0750 "$(dirname -- "${admin_socket}")"

    while IFS= read -r -d '' schema_file; do
        install -o root -g corrosion -m 0640 \
            -- "${schema_file}" "${schema_path}/$(basename -- "${schema_file}")"
        ((schema_count += 1))
    done < <(find "${schema_source}" -maxdepth 1 -type f -name '*.sql' -print0)
    ((schema_count > 0)) || fail "schema directory contains no .sql files"

    chown corrosion:corrosion "${rendered_config}"
    chmod 0600 "${rendered_config}"
    mv -f -- "${rendered_config}" /etc/corrosion/config.toml
    trap - EXIT

    install -o root -g root -m 0644 -- \
        "${SERVICE_SOURCE}" /etc/systemd/system/corrosion.service

    systemctl enable --now chrony.service
    systemctl daemon-reload
    systemctl enable corrosion.service

    printf 'corrosion_release=%s\n' "${CORROSION_RELEASE}"
    printf 'clock_gate_command=sudo %q clock-gate\n' "$0"
    printf 'corrosion_start_command=sudo %q start\n' "$0"
    printf 'next_step=configure_wg0\n'
}

case ${1:-} in
    install)
        shift
        install_corrosion "$@"
        ;;
    clock-gate)
        [[ $# -eq 1 ]] || fail "clock-gate takes no arguments"
        clock_gate
        ;;
    start)
        [[ $# -eq 1 ]] || fail "start takes no arguments"
        start_corrosion
        ;;
    *)
        fail "usage: $0 {install <binary> <sha256> <config-values.json> <schema-directory>|clock-gate|start}"
        ;;
esac
