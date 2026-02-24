package network

import (
	"fmt"
	"net/netip"
	"strings"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"

	"ployz/internal/check"
)

const ManagementCIDR = "fd8c:88ad:7f06::/48"

var managementPrefix = [6]byte{0xfd, 0x8c, 0x88, 0xad, 0x7f, 0x06}

func ManagementIPFromPublicKey(publicKey string) (netip.Addr, error) {
	key, err := wgtypes.ParseKey(strings.TrimSpace(publicKey))
	if err != nil {
		return netip.Addr{}, fmt.Errorf("parse wireguard public key: %w", err)
	}
	return ManagementIPFromWGKey(key), nil
}

func ManagementIPFromWGKey(publicKey wgtypes.Key) netip.Addr {
	var b [16]byte
	copy(b[:6], managementPrefix[:])
	copy(b[6:], publicKey[:10])
	result := netip.AddrFrom16(b)
	check.Assert(result.IsValid() && result.Is6(), "ManagementIPFromWGKey: result must be valid IPv6")
	return result
}
