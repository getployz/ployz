package ployz

import (
	"net/netip"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

const ManagementCIDR = "fd8c:88ad:7f06::/48"

var managementPrefix = [6]byte{0xfd, 0x8c, 0x88, 0xad, 0x7f, 0x06}

// ManagementIPFromKey derives the IPv6 management address from a WireGuard public key.
func ManagementIPFromKey(publicKey wgtypes.Key) netip.Addr {
	var b [16]byte
	copy(b[:6], managementPrefix[:])
	copy(b[6:], publicKey[:10])
	return netip.AddrFrom16(b)
}
