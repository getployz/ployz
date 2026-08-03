#!/usr/bin/env bash
# Usage:
#   sudo ./mesh.sh <wg-address-cidr> <mesh-cidr> <private-key-file> <wg-port> \
#     '<peer-public-key>|<endpoint>|<allowed-ips>' [...]
#
# At least one peer tuple is required; an empty peer set is refused before the
# live interface is touched.

set -euo pipefail

readonly WG_CONFIG=/etc/wireguard/wg0.conf
readonly GOSSIP_PORT=8787

fail() {
    printf 'mesh: %s\n' "$*" >&2
    exit 1
}

[[ ${EUID} -eq 0 ]] || fail "run as root"
[[ $# -ge 5 ]] ||
    fail "usage: $0 <wg-address-cidr> <mesh-cidr> <private-key-file> <wg-port> '<public-key>|<endpoint>|<allowed-ips>' [...]"

readonly WG_ADDRESS=$1
readonly MESH_CIDR=$2
readonly PRIVATE_KEY_FILE=$3
readonly WG_PORT=$4
shift 4
readonly -a PEERS=("$@")

if ! [[ ${WG_PORT} =~ ^[0-9]+$ ]] || ((WG_PORT < 1 || WG_PORT > 65535)); then
    fail "WireGuard port must be between 1 and 65535"
fi
[[ -f ${PRIVATE_KEY_FILE} ]] || fail "missing private-key file: ${PRIVATE_KEY_FILE}"
[[ $(stat -c '%u' -- "${PRIVATE_KEY_FILE}") -eq 0 ]] ||
    fail "private-key file must be owned by root"
if find "${PRIVATE_KEY_FILE}" -prune -perm /077 -print -quit | grep -q .; then
    fail "private-key file must not be accessible by group or other"
fi

for command in iptables python3 systemctl wg wg-quick; do
    command -v "${command}" >/dev/null || fail "required command is unavailable: ${command}"
done

python3 - "${WG_ADDRESS}" "${MESH_CIDR}" <<'PY'
import ipaddress
import sys

try:
    address = ipaddress.ip_interface(sys.argv[1])
    mesh = ipaddress.ip_network(sys.argv[2])
except ValueError:
    raise SystemExit("mesh: WireGuard address and mesh must be numeric CIDRs") from None
if address.version != 4 or mesh.version != 4:
    raise SystemExit("mesh: this spike expects an IPv4 WireGuard mesh")
if not address.ip.is_private or address.ip.is_loopback or address.ip.is_unspecified:
    raise SystemExit("mesh: WireGuard address must be private unicast")
if not mesh.is_private or address.ip not in mesh:
    raise SystemExit("mesh: WireGuard address must belong to the private mesh CIDR")
PY

private_key=$(<"${PRIVATE_KEY_FILE}")
[[ ${private_key} =~ ^[A-Za-z0-9+/]{43}=$ ]] ||
    fail "private-key file is not a WireGuard key"
printf '%s' "${private_key}" | wg pubkey >/dev/null ||
    fail "private-key file is not a WireGuard key"

umask 077
mesh_tmp_dir=$(mktemp -d /tmp/ployz-corrosion-mesh.XXXXXX)
config_tmp="${mesh_tmp_dir}/wg0.conf"
trap 'rm -f -- "${config_tmp:-}"; rmdir -- "${mesh_tmp_dir:-}" 2>/dev/null || true' EXIT

{
    printf '[Interface]\n'
    printf 'Address = %s\n' "${WG_ADDRESS}"
    printf 'PrivateKey = %s\n' "${private_key}"
    printf 'ListenPort = %s\n' "${WG_PORT}"
    printf 'MTU = 1280\n'
    printf 'PostUp = iptables -w -I INPUT 1 -i wg0 -s %s -p udp --dport %s -j ACCEPT\n' \
        "${MESH_CIDR}" "${GOSSIP_PORT}"
    printf 'PostUp = iptables -w -I INPUT 2 -i wg0 -p udp --dport %s -j DROP\n' \
        "${GOSSIP_PORT}"
    printf 'PostDown = iptables -w -D INPUT -i wg0 -s %s -p udp --dport %s -j ACCEPT 2>/dev/null || true\n' \
        "${MESH_CIDR}" "${GOSSIP_PORT}"
    printf 'PostDown = iptables -w -D INPUT -i wg0 -p udp --dport %s -j DROP 2>/dev/null || true\n' \
        "${GOSSIP_PORT}"

    for peer in "${PEERS[@]}"; do
        IFS='|' read -r peer_public_key endpoint allowed_ips extra <<<"${peer}"
        [[ -n ${peer_public_key} && -n ${endpoint} && -n ${allowed_ips} && -z ${extra} ]] ||
            fail "peer tuples must be '<public-key>|<endpoint>|<allowed-ips>'"
        [[ ${peer_public_key} =~ ^[A-Za-z0-9+/]{43}=$ ]] ||
            fail "peer public key is not a WireGuard key"
        python3 - "${endpoint}" "${allowed_ips}" "${MESH_CIDR}" <<'PY'
import ipaddress
import sys

endpoint, allowed_ips, mesh_cidr = sys.argv[1:]
host, separator, port = endpoint.rpartition(":")
if (
    not separator
    or not port.isascii()
    or not port.isdecimal()
    or not 1 <= int(port) <= 65535
):
    raise SystemExit("mesh: peer endpoint must be a numeric IPv4 socket address")
try:
    endpoint_ip = ipaddress.ip_address(host)
    mesh = ipaddress.ip_network(mesh_cidr)
    networks = [ipaddress.ip_network(item.strip()) for item in allowed_ips.split(",")]
except ValueError:
    raise SystemExit("mesh: peer endpoint and allowed-ips must be numeric") from None
if endpoint_ip.version != 4 or endpoint_ip.is_unspecified or endpoint_ip.is_multicast:
    raise SystemExit("mesh: peer endpoint must be a numeric IPv4 socket address")
if not networks or any(network.version != 4 or not network.subnet_of(mesh) for network in networks):
    raise SystemExit("mesh: peer allowed-ips must stay inside the mesh CIDR")
PY
        printf '\n[Peer]\n'
        printf 'PublicKey = %s\n' "${peer_public_key}"
        printf 'Endpoint = %s\n' "${endpoint}"
        printf 'AllowedIPs = %s\n' "${allowed_ips}"
        printf 'PersistentKeepalive = 25\n'
    done
} >"${config_tmp}"

wg-quick strip "${config_tmp}" >/dev/null ||
    fail "generated WireGuard configuration is invalid"

if systemctl is-active --quiet wg-quick@wg0.service; then
    systemctl stop wg-quick@wg0.service
fi
install -o root -g root -m 0600 -- "${config_tmp}" "${WG_CONFIG}"
rm -f -- "${config_tmp}"
rmdir -- "${mesh_tmp_dir}"
trap - EXIT

systemctl enable --now wg-quick@wg0.service
printf 'wireguard=ready interface=wg0 address=%s peers=%s\n' "${WG_ADDRESS}" "${#PEERS[@]}"
