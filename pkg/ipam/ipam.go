package ipam

import (
	"encoding/binary"
	"fmt"
	"math"
	"net/netip"
)

const SubnetBits = 24

func AllocateSubnet(network netip.Prefix, allocated []netip.Prefix) (netip.Prefix, error) {
	if !network.IsValid() {
		return netip.Prefix{}, fmt.Errorf("network cidr is required")
	}
	network = network.Masked()
	if !network.Addr().Is4() {
		return netip.Prefix{}, fmt.Errorf("only ipv4 network cidr is supported")
	}
	if SubnetBits < network.Bits() || SubnetBits > 32 {
		return netip.Prefix{}, fmt.Errorf("invalid subnet size /%d for network %s", SubnetBits, network)
	}

	start, end, err := PrefixRange4(network)
	if err != nil {
		return netip.Prefix{}, err
	}
	step := uint32(1) << (32 - SubnetBits)

	ranges := make([][2]uint32, 0, len(allocated))
	for _, p := range allocated {
		if !p.IsValid() || !p.Addr().Is4() {
			continue
		}
		if !network.Contains(p.Masked().Addr()) {
			continue
		}
		aStart, aEnd, rErr := PrefixRange4(p)
		if rErr != nil {
			continue
		}
		ranges = append(ranges, [2]uint32{aStart, aEnd})
	}

	for curr := start; curr <= end; {
		subnet := netip.PrefixFrom(Uint32ToAddr(curr), SubnetBits)
		sStart, sEnd, _ := PrefixRange4(subnet)

		overlap := false
		for _, r := range ranges {
			if RangesOverlap(sStart, sEnd, r[0], r[1]) {
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

	return netip.Prefix{}, fmt.Errorf("no available /%d subnet in %s", SubnetBits, network)
}

func PrefixRange4(p netip.Prefix) (uint32, uint32, error) {
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

func RangesOverlap(aStart, aEnd, bStart, bEnd uint32) bool {
	return !(aEnd < bStart || bEnd < aStart)
}

func Uint32ToAddr(v uint32) netip.Addr {
	var b [4]byte
	binary.BigEndian.PutUint32(b[:], v)
	return netip.AddrFrom4(b)
}
