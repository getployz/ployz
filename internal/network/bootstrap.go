package network

import "strings"

func NormalizeBootstrapAddrPort(raw string) string {
	return strings.TrimSpace(raw)
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
