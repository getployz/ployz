package machine

import (
	"errors"
	"fmt"
	"net/netip"
	"os"
	"path/filepath"

	"ployz/pkg/ipam"
)

func ensureUniqueHostCIDR(cfg Config) error {
	if !cfg.NetworkCIDR.IsValid() || cfg.DataRoot == "" {
		return nil
	}
	entries, err := os.ReadDir(cfg.DataRoot)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil
		}
		return fmt.Errorf("read data root: %w", err)
	}

	for _, e := range entries {
		if !e.IsDir() || e.Name() == cfg.Network {
			continue
		}
		otherStatePath := filepath.Join(cfg.DataRoot, e.Name())
		s, sErr := loadState(otherStatePath)
		if sErr != nil {
			continue
		}
		otherCIDR := defaultNetworkPrefix
		if s.CIDR != "" {
			parsed, pErr := netip.ParsePrefix(s.CIDR)
			if pErr == nil {
				otherCIDR = parsed
			}
		}
		overlap, oErr := prefixesOverlap(cfg.NetworkCIDR, otherCIDR)
		if oErr != nil {
			continue
		}
		if overlap {
			return fmt.Errorf(
				"network %q CIDR %s overlaps with network %q CIDR %s on this host",
				cfg.Network,
				cfg.NetworkCIDR,
				e.Name(),
				otherCIDR,
			)
		}
	}
	return nil
}

func prefixesOverlap(a, b netip.Prefix) (bool, error) {
	a = a.Masked()
	b = b.Masked()
	if !a.IsValid() || !b.IsValid() {
		return false, fmt.Errorf("invalid prefix")
	}
	if a.Addr().Is4() && b.Addr().Is4() {
		aStart, aEnd, err := ipam.PrefixRange4(a)
		if err != nil {
			return false, err
		}
		bStart, bEnd, err := ipam.PrefixRange4(b)
		if err != nil {
			return false, err
		}
		return ipam.RangesOverlap(aStart, aEnd, bStart, bEnd), nil
	}
	if a.Addr().Is6() && b.Addr().Is6() {
		return a.Contains(b.Addr()) || b.Contains(a.Addr()), nil
	}
	return false, nil
}
