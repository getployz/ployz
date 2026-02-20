package machine

import (
	"fmt"
	"net/netip"
	"strings"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

func ManagementIPFromPublicKey(publicKey string) (netip.Addr, error) {
	key, err := wgtypes.ParseKey(strings.TrimSpace(publicKey))
	if err != nil {
		return netip.Addr{}, fmt.Errorf("parse wireguard public key: %w", err)
	}
	return managementIPFromWGKey(key), nil
}

func managementIPFromWGKey(publicKey wgtypes.Key) netip.Addr {
	var bytes [16]byte
	bytes[0] = 0xfd
	bytes[1] = 0xcc
	copy(bytes[2:], publicKey[:14])
	return netip.AddrFrom16(bytes)
}
