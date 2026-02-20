package machine

import (
	"net/netip"

	"ployz/pkg/ipam"
)

const (
	DefaultNetworkCIDR = "10.210.0.0/16"
	MachineSubnetBits  = ipam.SubnetBits
)

var defaultNetworkPrefix = netip.MustParsePrefix(DefaultNetworkCIDR)

func defaultNetwork() netip.Prefix {
	return defaultNetworkPrefix
}

func allocateMachineSubnet(network netip.Prefix, allocated []netip.Prefix) (netip.Prefix, error) {
	return ipam.AllocateSubnet(network, allocated)
}
