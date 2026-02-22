package ipam

import (
	"net/netip"
	"testing"
)

func FuzzAllocateSubnet(f *testing.F) {
	f.Add("10.210.0.0/16")
	f.Add("10.0.0.0/8")
	f.Add("192.168.0.0/16")

	f.Fuzz(func(t *testing.T, networkStr string) {
		network, err := netip.ParsePrefix(networkStr)
		if err != nil {
			return
		}
		if !network.Addr().Is4() {
			return
		}
		if network.Bits() >= SubnetBits {
			return
		}

		result, err := AllocateSubnet(network, nil)
		if err != nil {
			return
		}

		// Result within network CIDR.
		if !network.Contains(result.Addr()) {
			t.Errorf("result %v not within %v", result, network)
		}

		// Result has correct prefix length.
		if result.Bits() != SubnetBits {
			t.Errorf("result prefix length %d, want %d", result.Bits(), SubnetBits)
		}
	})
}
