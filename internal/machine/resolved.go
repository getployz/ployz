package machine

import (
	"fmt"
	"net/netip"
)

type Resolved Config

func (r Resolved) Config() Config {
	return Config(r)
}

func Resolve(cfg Config, s *State) (Resolved, error) {
	norm, err := NormalizeConfig(cfg)
	if err != nil {
		return Resolved{}, err
	}
	if s == nil {
		return Resolved(norm), nil
	}

	if norm.Network == "" {
		norm.Network = s.Network
	}
	if norm.WGInterface == "" {
		norm.WGInterface = s.WGInterface
	}
	if norm.WGPort == 0 {
		norm.WGPort = s.WGPort
	}
	if norm.DockerNetwork == "" {
		norm.DockerNetwork = s.DockerNetwork
	}
	if norm.CorrosionName == "" {
		norm.CorrosionName = s.CorrosionName
	}
	if norm.CorrosionImg == "" {
		norm.CorrosionImg = s.CorrosionImg
	}
	if !norm.NetworkCIDR.IsValid() {
		if s.CIDR != "" {
			cidr, err := netip.ParsePrefix(s.CIDR)
			if err != nil {
				return Resolved{}, fmt.Errorf("parse cidr from state: %w", err)
			}
			norm.NetworkCIDR = cidr
		} else {
			norm.NetworkCIDR = defaultNetwork()
		}
	}
	if len(norm.CorrosionBootstrap) == 0 && len(s.Bootstrap) > 0 {
		norm.CorrosionBootstrap = append([]string(nil), s.Bootstrap...)
	}
	if norm.AdvertiseEP == "" {
		norm.AdvertiseEP = s.Advertise
	}
	if !norm.Subnet.IsValid() {
		subnet, err := netip.ParsePrefix(s.Subnet)
		if err != nil {
			return Resolved{}, fmt.Errorf("parse subnet from state: %w", err)
		}
		norm.Subnet = subnet
	}
	mgmt, err := netip.ParseAddr(s.Management)
	if err != nil {
		return Resolved{}, fmt.Errorf("parse management IP from state: %w", err)
	}
	norm.Management = mgmt
	refreshCorrosionGossipAddr(&norm)
	norm.CorrosionGossipAP = netip.AddrPortFrom(norm.CorrosionGossipIP, uint16(norm.CorrosionGossip))

	return Resolved(norm), nil
}
