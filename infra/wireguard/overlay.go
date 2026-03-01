package wireguard

import "net/netip"

// HostPrefix returns a single-host prefix for the given IP address
// (/128 for IPv6, /32 for IPv4). This is used throughout the wireguard
// subsystem to derive allowed-IP and route entries from overlay addresses.
func HostPrefix(ip netip.Addr) netip.Prefix {
	bits := 32
	if ip.Is6() {
		bits = 128
	}
	return netip.PrefixFrom(ip, bits)
}

// PeerOwner identifies who manages a WireGuard peer. Convergence
// (SetPeers) only removes peers it owns (PeerOwnerMesh). Bridge and
// future session peers survive convergence cycles.
//
// This lives in the shared wireguard package because peer ownership
// applies to all WireGuard backends (kernel, container, future connect).
type PeerOwner string

const (
	PeerOwnerMesh   PeerOwner = "mesh"   // convergence loop
	PeerOwnerBridge PeerOwner = "bridge" // local overlay bridge
)