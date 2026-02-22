package network

import (
	"fmt"
	"net/netip"
	"strings"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

const ManagementCIDR = "fd8c:88ad:7f06::/48"

var (
	managementPrefix       = [6]byte{0xfd, 0x8c, 0x88, 0xad, 0x7f, 0x06}
	legacyManagementPrefix = [2]byte{0xfd, 0xcc}
)

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
	return netip.AddrFrom16(b)
}

func MigrateLegacyManagementAddr(addr netip.Addr) (netip.Addr, bool) {
	if !addr.IsValid() || !addr.Is6() {
		return netip.Addr{}, false
	}
	raw := addr.As16()
	if raw[0] != legacyManagementPrefix[0] || raw[1] != legacyManagementPrefix[1] {
		return netip.Addr{}, false
	}

	var b [16]byte
	copy(b[:6], managementPrefix[:])
	copy(b[6:], raw[2:12])
	return netip.AddrFrom16(b), true
}
