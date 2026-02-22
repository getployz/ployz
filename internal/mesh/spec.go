package mesh

import (
	"fmt"
	"net/netip"
	"strings"

	"ployz/pkg/sdk/defaults"
	"ployz/pkg/sdk/types"
)

// ConfigFromSpec converts a user-facing NetworkSpec into a normalized Config.
func ConfigFromSpec(spec types.NetworkSpec) (Config, error) {
	cfg := Config{
		Network:           defaults.NormalizeNetwork(spec.Network),
		DataRoot:          strings.TrimSpace(spec.DataRoot),
		AdvertiseEndpoint: strings.TrimSpace(spec.AdvertiseEndpoint),
		WGPort:            spec.WGPort,
		CorrosionMemberID: spec.CorrosionMemberID,
		CorrosionAPIToken: strings.TrimSpace(spec.CorrosionAPIToken),
		HelperImage:       strings.TrimSpace(spec.HelperImage),
	}
	for _, bs := range spec.Bootstrap {
		bs = NormalizeBootstrapAddrPort(bs)
		if bs == "" {
			continue
		}
		cfg.CorrosionBootstrap = append(cfg.CorrosionBootstrap, bs)
	}

	if cidr := strings.TrimSpace(spec.NetworkCIDR); cidr != "" {
		pfx, err := netip.ParsePrefix(cidr)
		if err != nil {
			return Config{}, fmt.Errorf("parse network cidr: %w", err)
		}
		cfg.NetworkCIDR = pfx
	}
	if subnet := strings.TrimSpace(spec.Subnet); subnet != "" {
		pfx, err := netip.ParsePrefix(subnet)
		if err != nil {
			return Config{}, fmt.Errorf("parse subnet: %w", err)
		}
		cfg.Subnet = pfx
	}
	if mgmt := strings.TrimSpace(spec.ManagementIP); mgmt != "" {
		addr, err := netip.ParseAddr(mgmt)
		if err != nil {
			return Config{}, fmt.Errorf("parse management ip: %w", err)
		}
		cfg.Management = addr
	}

	return NormalizeConfig(cfg)
}
