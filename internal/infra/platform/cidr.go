package platform

import (
	"errors"
	"fmt"
	"net/netip"
	"os"
	"path/filepath"

	"ployz/pkg/ipam"
)

// CIDRLoader loads the CIDR string for a network stored in the given data directory.
type CIDRLoader func(dataDir string) (string, error)

func EnsureUniqueHostCIDR(networkCIDR netip.Prefix, dataRoot, network string, defaultCIDR netip.Prefix, loadCIDR CIDRLoader) error {
	if !networkCIDR.IsValid() || dataRoot == "" {
		return nil
	}
	entries, err := os.ReadDir(dataRoot)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil
		}
		return fmt.Errorf("read data root: %w", err)
	}

	for _, e := range entries {
		if !e.IsDir() || e.Name() == network {
			continue
		}
		otherCIDRStr, sErr := loadCIDR(filepath.Join(dataRoot, e.Name()))
		if sErr != nil {
			continue
		}
		otherCIDR := defaultCIDR
		if otherCIDRStr != "" {
			parsed, pErr := netip.ParsePrefix(otherCIDRStr)
			if pErr == nil {
				otherCIDR = parsed
			}
		}
		overlap, oErr := PrefixesOverlap(networkCIDR, otherCIDR)
		if oErr != nil {
			continue
		}
		if overlap {
			return fmt.Errorf(
				"network %q CIDR %s overlaps with network %q CIDR %s on this host",
				network,
				networkCIDR,
				e.Name(),
				otherCIDR,
			)
		}
	}
	return nil
}

func PrefixesOverlap(a, b netip.Prefix) (bool, error) {
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
