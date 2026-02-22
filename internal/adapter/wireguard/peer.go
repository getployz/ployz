package wireguard

import (
	"net/netip"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

// PeerConfig holds parsed peer configuration for WireGuard setup.
type PeerConfig struct {
	PublicKey       wgtypes.Key
	Endpoint        *netip.AddrPort
	AllowedPrefixes []netip.Prefix
}
