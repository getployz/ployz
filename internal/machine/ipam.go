package machine

import (
	"encoding/binary"
	"fmt"
	"math"
	"net/netip"
)

const (
	DefaultNetworkCIDR = "10.210.0.0/16"
	MachineSubnetBits  = 24
)

var defaultNetworkPrefix = netip.MustParsePrefix(DefaultNetworkCIDR)

func defaultNetwork() netip.Prefix {
	return defaultNetworkPrefix
}

func allocateMachineSubnet(network netip.Prefix, allocated []netip.Prefix) (netip.Prefix, error) {
	if !network.IsValid() {
		return netip.Prefix{}, fmt.Errorf("network cidr is required")
	}
	network = network.Masked()
	if !network.Addr().Is4() {
		return netip.Prefix{}, fmt.Errorf("only ipv4 network cidr is supported")
	}
	if MachineSubnetBits < network.Bits() || MachineSubnetBits > 32 {
		return netip.Prefix{}, fmt.Errorf("invalid subnet size /%d for network %s", MachineSubnetBits, network)
	}

	start, end, err := prefixRange4(network)
	if err != nil {
		return netip.Prefix{}, err
	}
	step := uint32(1) << (32 - MachineSubnetBits)

	alloc := make([][2]uint32, 0, len(allocated))
	for _, p := range allocated {
		if !p.IsValid() || !p.Addr().Is4() {
			continue
		}
		if !network.Contains(p.Masked().Addr()) {
			continue
		}
		aStart, aEnd, rErr := prefixRange4(p)
		if rErr != nil {
			continue
		}
		alloc = append(alloc, [2]uint32{aStart, aEnd})
	}

	for curr := start; curr <= end; {
		subnet := netip.PrefixFrom(uint32ToAddr(curr), MachineSubnetBits)
		sStart, sEnd, _ := prefixRange4(subnet)

		overlap := false
		for _, r := range alloc {
			if rangesOverlap(sStart, sEnd, r[0], r[1]) {
				overlap = true
				break
			}
		}
		if !overlap {
			return subnet, nil
		}

		if curr > math.MaxUint32-step {
			break
		}
		curr += step
	}

	return netip.Prefix{}, fmt.Errorf("no available /%d subnet in %s", MachineSubnetBits, network)
}

func rangesOverlap(aStart, aEnd, bStart, bEnd uint32) bool {
	return !(aEnd < bStart || bEnd < aStart)
}

func prefixRange4(p netip.Prefix) (uint32, uint32, error) {
	p = p.Masked()
	if !p.Addr().Is4() {
		return 0, 0, fmt.Errorf("prefix %s is not ipv4", p)
	}
	b := p.Addr().As4()
	start := binary.BigEndian.Uint32(b[:])
	hostBits := 32 - p.Bits()
	if hostBits <= 0 {
		return start, start, nil
	}
	if hostBits >= 32 {
		return 0, math.MaxUint32, nil
	}
	size := uint32(1) << hostBits
	return start, start + size - 1, nil
}

func uint32ToAddr(v uint32) netip.Addr {
	var b [4]byte
	binary.BigEndian.PutUint32(b[:], v)
	return netip.AddrFrom4(b)
}
