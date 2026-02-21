package wireguard

import (
	"fmt"
	"net/netip"
	"strings"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

// PeerConfig holds parsed peer configuration for WireGuard setup.
type PeerConfig struct {
	PublicKey       wgtypes.Key
	Endpoint        *netip.AddrPort
	AllowedPrefixes []netip.Prefix
}

func ManagementIPFromPublicKey(publicKey string) (netip.Addr, error) {
	key, err := wgtypes.ParseKey(strings.TrimSpace(publicKey))
	if err != nil {
		return netip.Addr{}, fmt.Errorf("parse wireguard public key: %w", err)
	}
	return ManagementIPFromWGKey(key), nil
}

func ManagementIPFromWGKey(publicKey wgtypes.Key) netip.Addr {
	var bytes [16]byte
	bytes[0] = 0xfd
	bytes[1] = 0xcc
	copy(bytes[2:], publicKey[:14])
	return netip.AddrFrom16(bytes)
}
