package wireguard

import (
	"fmt"
	"net/netip"
	"strings"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

const (
	ManagementCIDR = "fd8c:88ad:7f06::/48"

	managementPrefixByte0 = 0xfd
	managementPrefixByte1 = 0x8c
	managementPrefixByte2 = 0x88
	managementPrefixByte3 = 0xad
	managementPrefixByte4 = 0x7f
	managementPrefixByte5 = 0x06

	legacyManagementPrefixByte0 = 0xfd
	legacyManagementPrefixByte1 = 0xcc
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
	bytes[0] = managementPrefixByte0
	bytes[1] = managementPrefixByte1
	bytes[2] = managementPrefixByte2
	bytes[3] = managementPrefixByte3
	bytes[4] = managementPrefixByte4
	bytes[5] = managementPrefixByte5
	copy(bytes[6:], publicKey[:10])
	return netip.AddrFrom16(bytes)
}

func MigrateLegacyManagementAddr(addr netip.Addr) (netip.Addr, bool) {
	if !addr.IsValid() || !addr.Is6() {
		return netip.Addr{}, false
	}
	bytes := addr.As16()
	if bytes[0] != legacyManagementPrefixByte0 || bytes[1] != legacyManagementPrefixByte1 {
		return netip.Addr{}, false
	}

	var migrated [16]byte
	migrated[0] = managementPrefixByte0
	migrated[1] = managementPrefixByte1
	migrated[2] = managementPrefixByte2
	migrated[3] = managementPrefixByte3
	migrated[4] = managementPrefixByte4
	migrated[5] = managementPrefixByte5
	copy(migrated[6:], bytes[2:12])
	return netip.AddrFrom16(migrated), true
}
