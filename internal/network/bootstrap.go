package network

import (
	"net/netip"
	"strings"

	"ployz/internal/adapter/wireguard"
)

func NormalizeBootstrapAddrPort(raw string) string {
	addrPort := strings.TrimSpace(raw)
	if addrPort == "" {
		return ""
	}

	parsed, err := netip.ParseAddrPort(addrPort)
	if err != nil {
		return addrPort
	}

	if migrated, ok := wireguard.MigrateLegacyManagementAddr(parsed.Addr()); ok {
		return netip.AddrPortFrom(migrated, parsed.Port()).String()
	}
	return addrPort
}

func normalizeBootstrapAddrs(values []string) []string {
	if len(values) == 0 {
		return nil
	}

	out := make([]string, 0, len(values))
	seen := make(map[string]struct{}, len(values))
	for _, value := range values {
		normalized := NormalizeBootstrapAddrPort(value)
		if normalized == "" {
			continue
		}
		if _, ok := seen[normalized]; ok {
			continue
		}
		seen[normalized] = struct{}{}
		out = append(out, normalized)
	}
	return out
}
