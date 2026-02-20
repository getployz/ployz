package machine

import (
	"net/netip"

	"ployz/pkg/ipam"
)

var defaultNetworkPrefix = netip.MustParsePrefix("10.210.0.0/16")

func defaultNetwork() netip.Prefix {
	return defaultNetworkPrefix
}

func allocateMachineSubnet(network netip.Prefix, allocated []netip.Prefix) (netip.Prefix, error) {
	return ipam.AllocateSubnet(network, allocated)
}
