package registry

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/netip"
	"strconv"
	"strings"
)

const (
	networkConfigTable = "network_config"
	networkConfigKey   = "cidr"
	machinesTable      = "machines"
)

var ErrConflict = errors.New("registry version conflict")

type Store struct {
	apiAddr netip.AddrPort
}

type NetworkConfigRow struct {
	CIDR string
}

type MachineRow struct {
	ID         string
	PublicKey  string
	Subnet     string
	Management string
	Endpoint   string
	UpdatedAt  string
	Version    int64
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
    updated_at TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 1
)`, machinesTable)
	if err := s.exec(ctx, query); err != nil {
		return err
	}
	if err := s.exec(ctx, fmt.Sprintf("ALTER TABLE %s ADD COLUMN version INTEGER NOT NULL DEFAULT 1", machinesTable)); err != nil {
		errMsg := strings.ToLower(strings.TrimSpace(err.Error()))
		if !strings.Contains(errMsg, "duplicate column") {
			return err
		}
	}
	return nil
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
	return s.UpsertMachine(ctx, "", row, 0)
}

func (s Store) UpsertMachine(ctx context.Context, _ string, row MachineRow, expectedVersion int64) error {
	row.ID = strings.TrimSpace(row.ID)
	if row.ID == "" {
		return fmt.Errorf("machine id is required")
	}

	current, exists, err := s.machineByID(ctx, row.ID)
	if err != nil {
		return err
	}

	if exists {
		if expectedVersion > 0 && current.Version != expectedVersion {
			return ErrConflict
		}
		if current.PublicKey == row.PublicKey &&
			current.Subnet == row.Subnet &&
			current.Management == row.Management &&
			current.Endpoint == row.Endpoint {
			return nil
		}
		row.Version = current.Version + 1
		query := fmt.Sprintf(
			"UPDATE %s SET public_key = ?, subnet = ?, management_ip = ?, endpoint = ?, updated_at = ?, version = ? WHERE id = ?",
			machinesTable,
		)
		return s.exec(ctx, query, row.PublicKey, row.Subnet, row.Management, row.Endpoint, row.UpdatedAt, row.Version, row.ID)
	}

	if expectedVersion > 0 {
		return ErrConflict
	}
	if row.Version <= 0 {
		row.Version = 1
	}
	query := fmt.Sprintf(
		"INSERT INTO %s (id, public_key, subnet, management_ip, endpoint, updated_at, version) VALUES (?, ?, ?, ?, ?, ?, ?)",
		machinesTable,
	)
	return s.exec(ctx, query, row.ID, row.PublicKey, row.Subnet, row.Management, row.Endpoint, row.UpdatedAt, row.Version)
}

func (s Store) RemoveMachine(ctx context.Context, _ string, id string) error {
	return s.DeleteMachine(ctx, id)
}

func (s Store) DeleteByEndpointExceptID(ctx context.Context, endpoint, id string) error {
	query := fmt.Sprintf("DELETE FROM %s WHERE endpoint = ? AND id <> ?", machinesTable)
	return s.exec(ctx, query, endpoint, id)
}

func (s Store) DeleteMachine(ctx context.Context, machineID string) error {
	query := fmt.Sprintf("DELETE FROM %s WHERE id = ? OR endpoint = ?", machinesTable)
	return s.exec(ctx, query, machineID, machineID)
}

func (s Store) ListMachines(ctx context.Context, _ string) ([]MachineRow, error) {
	return s.ListMachineRows(ctx)
}

func (s Store) ListMachineRows(ctx context.Context) ([]MachineRow, error) {
	query := fmt.Sprintf("SELECT id, public_key, subnet, management_ip, endpoint, updated_at, version FROM %s ORDER BY id", machinesTable)
	rows, err := s.query(ctx, query)
	if err != nil {
		return nil, err
	}
	out := make([]MachineRow, 0, len(rows))
	for _, row := range rows {
		r, rErr := decodeMachineRow(row)
		if rErr != nil {
			return nil, rErr
		}
		out = append(out, r)
	}
	return out, nil
}

func (s Store) GetNetworkConfig(ctx context.Context, _ string) (NetworkConfigRow, error) {
	value, err := s.networkConfigValue(ctx, networkConfigKey)
	if err != nil {
		return NetworkConfigRow{}, err
	}
	return NetworkConfigRow{CIDR: value}, nil
}

func (s Store) EnsureNetworkConfig(ctx context.Context, _ string, cfg NetworkConfigRow) error {
	if strings.TrimSpace(cfg.CIDR) == "" {
		return nil
	}
	return s.setNetworkConfigValue(ctx, networkConfigKey, cfg.CIDR)
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

func (s Store) machineByID(ctx context.Context, id string) (MachineRow, bool, error) {
	query := fmt.Sprintf(
		"SELECT id, public_key, subnet, management_ip, endpoint, updated_at, version FROM %s WHERE id = ?",
		machinesTable,
	)
	rows, err := s.query(ctx, query, id)
	if err != nil {
		return MachineRow{}, false, err
	}
	if len(rows) == 0 {
		return MachineRow{}, false, nil
	}
	out, err := decodeMachineRow(rows[0])
	if err != nil {
		return MachineRow{}, false, err
	}
	return out, true, nil
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

func decodeInt64(raw json.RawMessage, label string) (int64, error) {
	var n int64
	if err := json.Unmarshal(raw, &n); err == nil {
		return n, nil
	}
	var f float64
	if err := json.Unmarshal(raw, &f); err == nil {
		return int64(f), nil
	}
	var s string
	if err := json.Unmarshal(raw, &s); err == nil {
		i, convErr := strconv.ParseInt(strings.TrimSpace(s), 10, 64)
		if convErr == nil {
			return i, nil
		}
	}
	return 0, fmt.Errorf("decode %s", label)
}
