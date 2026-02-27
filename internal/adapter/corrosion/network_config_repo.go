package corrosion

import (
	"context"
	"fmt"
	"net/netip"
	"strings"

	"ployz/internal/network"
)

var _ network.NetworkConfigRegistry = NetworkConfigRepo{}

// NetworkConfigRepo provides network_config-table operations over Corrosion.
type NetworkConfigRepo struct {
	client corrosionClient
}

// NewNetworkConfigRepo creates a network-config repository from Corrosion API coordinates.
func NewNetworkConfigRepo(apiAddr netip.AddrPort, apiToken string) NetworkConfigRepo {
	return NewStore(apiAddr, apiToken).NetworkConfig()
}

// NetworkConfig returns a network-config scoped repository backed by this Store.
func (s Store) NetworkConfig() NetworkConfigRepo {
	return NetworkConfigRepo{client: s.clientOrDefault()}
}

func (r NetworkConfigRepo) EnsureNetworkConfigTable(ctx context.Context) error {
	query := fmt.Sprintf(`
CREATE TABLE IF NOT EXISTS %s (
    key TEXT NOT NULL PRIMARY KEY,
    value TEXT NOT NULL
)`, networkConfigTable)
	return r.client.exec(ctx, query)
}

func (r NetworkConfigRepo) EnsureNetworkCIDR(
	ctx context.Context,
	requested netip.Prefix,
	fallbackCIDR string,
	defaultCIDR netip.Prefix,
) (netip.Prefix, error) {
	current, err := r.networkConfigValue(ctx, networkConfigKey)
	if err != nil {
		return netip.Prefix{}, err
	}

	if strings.TrimSpace(current) == "" {
		cidr := requested
		if !cidr.IsValid() && strings.TrimSpace(fallbackCIDR) != "" {
			parsed, parseErr := netip.ParsePrefix(fallbackCIDR)
			if parseErr == nil {
				cidr = parsed
			}
		}
		if !cidr.IsValid() {
			cidr = defaultCIDR
		}
		if err := r.setNetworkConfigValue(ctx, networkConfigKey, cidr.String()); err != nil {
			return netip.Prefix{}, err
		}
		return cidr, nil
	}

	parsed, err := netip.ParsePrefix(current)
	if err != nil {
		return netip.Prefix{}, fmt.Errorf("parse network cidr from corrosion: %w", err)
	}
	if requested.IsValid() && requested.String() != parsed.String() {
		return netip.Prefix{}, fmt.Errorf("network already uses CIDR %s, requested %s", parsed, requested)
	}
	return parsed, nil
}

func (r NetworkConfigRepo) networkConfigValue(ctx context.Context, key string) (string, error) {
	rows, err := r.client.query(ctx, fmt.Sprintf("SELECT value FROM %s WHERE key = ?", networkConfigTable), key)
	if err != nil {
		return "", err
	}
	if len(rows) == 0 || len(rows[0]) == 0 {
		return "", nil
	}
	return decodeString(rows[0][0], "network config value")
}

func (r NetworkConfigRepo) setNetworkConfigValue(ctx context.Context, key, value string) error {
	query := fmt.Sprintf("INSERT OR REPLACE INTO %s (key, value) VALUES (?, ?)", networkConfigTable)
	return r.client.exec(ctx, query, key, value)
}
