package network

import (
	"fmt"
	"net/netip"
)

func Resolve(cfg Config, s *State) (Config, error) {
	norm, err := NormalizeConfig(cfg)
	if err != nil {
		return Config{}, err
	}
	if s == nil {
		return norm, nil
	}

	// NormalizeConfig guarantees Network, WGInterface, WGPort, DockerNetwork,
	// CorrosionName, and CorrosionImage are always populated, so only fields
	// that NormalizeConfig cannot derive need state fallbacks.

	if norm.CorrosionMemberID == 0 {
		norm.CorrosionMemberID = s.CorrosionMemberID
	}
	if norm.CorrosionAPIToken == "" {
		norm.CorrosionAPIToken = s.CorrosionAPIToken
	}
	if !norm.NetworkCIDR.IsValid() {
		if s.CIDR != "" {
			cidr, err := netip.ParsePrefix(s.CIDR)
			if err != nil {
				return Config{}, fmt.Errorf("parse cidr from state: %w", err)
			}
			norm.NetworkCIDR = cidr
		} else {
			norm.NetworkCIDR = defaultNetworkPrefix
		}
	}
	if len(norm.CorrosionBootstrap) == 0 && len(s.Bootstrap) > 0 {
		norm.CorrosionBootstrap = normalizeBootstrapAddrs(s.Bootstrap)
	}
	if norm.AdvertiseEndpoint == "" {
		norm.AdvertiseEndpoint = s.Advertise
	}
	if !norm.Subnet.IsValid() {
		subnet, err := netip.ParsePrefix(s.Subnet)
		if err != nil {
			return Config{}, fmt.Errorf("parse subnet from state: %w", err)
		}
		norm.Subnet = subnet
	}
	mgmt, err := netip.ParseAddr(s.Management)
	if err != nil {
		return Config{}, fmt.Errorf("parse management IP from state: %w", err)
	}
	norm.Management = mgmt
	refreshCorrosionGossipAddr(&norm)
	norm.CorrosionGossipAddrPort = netip.AddrPortFrom(norm.CorrosionGossipIP, uint16(norm.CorrosionGossipPort))

	return norm, nil
}
