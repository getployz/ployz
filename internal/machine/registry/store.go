package registry

import (
	"context"
	"encoding/json"
	"fmt"
	"net/netip"
	"strings"
)

const (
	networkConfigTable = "network_config"
	networkConfigKey   = "cidr"
	machinesTable      = "machines"
)

type Store struct {
	apiAddr netip.AddrPort
}

type MachineRow struct {
	ID         string
	PublicKey  string
	Subnet     string
	Management string
	Endpoint   string
	UpdatedAt  string
}

func New(apiAddr netip.AddrPort) Store {
	return Store{apiAddr: apiAddr}
}

func (s Store) EnsureMachineTable(ctx context.Context) error {
	query := fmt.Sprintf(`
CREATE TABLE IF NOT EXISTS %s (
    id TEXT NOT NULL PRIMARY KEY,
    public_key TEXT NOT NULL,
    subnet TEXT NOT NULL,
    management_ip TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    updated_at TEXT NOT NULL
)`, machinesTable)
	return s.exec(ctx, query)
}

func (s Store) EnsureNetworkConfigTable(ctx context.Context) error {
	query := fmt.Sprintf(`
CREATE TABLE IF NOT EXISTS %s (
    key TEXT NOT NULL PRIMARY KEY,
    value TEXT NOT NULL
)`, networkConfigTable)
	return s.exec(ctx, query)
}

func (s Store) EnsureNetworkCIDR(
	ctx context.Context,
	requested netip.Prefix,
	fallbackCIDR string,
	defaultCIDR netip.Prefix,
) (netip.Prefix, error) {
	current, err := s.networkConfigValue(ctx, networkConfigKey)
	if err != nil {
		return netip.Prefix{}, err
	}

	if strings.TrimSpace(current) == "" {
		cidr := requested
		if !cidr.IsValid() && strings.TrimSpace(fallbackCIDR) != "" {
			parsed, pErr := netip.ParsePrefix(fallbackCIDR)
			if pErr == nil {
				cidr = parsed
			}
		}
		if !cidr.IsValid() {
			cidr = defaultCIDR
		}
		if err := s.setNetworkConfigValue(ctx, networkConfigKey, cidr.String()); err != nil {
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

func (s Store) RegisterMachine(ctx context.Context, row MachineRow) error {
	query := fmt.Sprintf(
		"INSERT OR REPLACE INTO %s (id, public_key, subnet, management_ip, endpoint, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
		machinesTable,
	)
	return s.exec(ctx, query, row.ID, row.PublicKey, row.Subnet, row.Management, row.Endpoint, row.UpdatedAt)
}

func (s Store) DeleteByEndpointExceptID(ctx context.Context, endpoint, id string) error {
	query := fmt.Sprintf("DELETE FROM %s WHERE endpoint = ? AND id <> ?", machinesTable)
	return s.exec(ctx, query, endpoint, id)
}

func (s Store) DeleteMachine(ctx context.Context, machineID string) error {
	query := fmt.Sprintf("DELETE FROM %s WHERE id = ? OR endpoint = ?", machinesTable)
	return s.exec(ctx, query, machineID, machineID)
}

func (s Store) ListMachineRows(ctx context.Context) ([]MachineRow, error) {
	query := fmt.Sprintf("SELECT id, public_key, subnet, management_ip, endpoint, updated_at FROM %s ORDER BY id", machinesTable)
	rows, err := s.query(ctx, query)
	if err != nil {
		return nil, err
	}
	out := make([]MachineRow, 0, len(rows))
	for _, row := range rows {
		if len(row) != 6 {
			continue
		}
		var r MachineRow
		if r.ID, err = decodeString(row[0], "machine id"); err != nil {
			return nil, err
		}
		if r.PublicKey, err = decodeString(row[1], "machine public key"); err != nil {
			return nil, err
		}
		if r.Subnet, err = decodeString(row[2], "machine subnet"); err != nil {
			return nil, err
		}
		if r.Management, err = decodeString(row[3], "machine management ip"); err != nil {
			return nil, err
		}
		if r.Endpoint, err = decodeString(row[4], "machine endpoint"); err != nil {
			return nil, err
		}
		if r.UpdatedAt, err = decodeString(row[5], "machine updated_at"); err != nil {
			return nil, err
		}
		out = append(out, r)
	}
	return out, nil
}

func (s Store) networkConfigValue(ctx context.Context, key string) (string, error) {
	rows, err := s.query(ctx, fmt.Sprintf("SELECT value FROM %s WHERE key = ?", networkConfigTable), key)
	if err != nil {
		return "", err
	}
	if len(rows) == 0 || len(rows[0]) == 0 {
		return "", nil
	}
	value, err := decodeString(rows[0][0], "network config value")
	if err != nil {
		return "", err
	}
	return value, nil
}

func (s Store) setNetworkConfigValue(ctx context.Context, key, value string) error {
	query := fmt.Sprintf("INSERT OR REPLACE INTO %s (key, value) VALUES (?, ?)", networkConfigTable)
	return s.exec(ctx, query, key, value)
}

func decodeString(raw json.RawMessage, label string) (string, error) {
	var s string
	if err := json.Unmarshal(raw, &s); err == nil {
		return s, nil
	}
	var p *string
	if err := json.Unmarshal(raw, &p); err == nil {
		if p == nil {
			return "", nil
		}
		return *p, nil
	}
	return "", fmt.Errorf("decode %s", label)
}
