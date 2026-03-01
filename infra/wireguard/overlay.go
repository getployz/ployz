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