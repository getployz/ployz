package corrosion

import (
	"context"
	"encoding/json"
	"fmt"
	"net/netip"
	"strconv"
	"strings"

	"ployz/internal/mesh"
)

const (
	networkConfigTable = "network_config"
	networkConfigKey   = "cidr"
	machinesTable      = "machines"
	heartbeatsTable    = "heartbeats"
)

type Store struct {
	apiAddr  netip.AddrPort
	apiToken string
}

func NewStore(apiAddr netip.AddrPort, apiToken string) Store {
	return Store{apiAddr: apiAddr, apiToken: strings.TrimSpace(apiToken)}
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

func (s Store) UpsertMachine(ctx context.Context, row mesh.MachineRow, expectedVersion int64) error {
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
			return mesh.ErrConflict
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
		return mesh.ErrConflict
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

func (s Store) DeleteByEndpointExceptID(ctx context.Context, endpoint, id string) error {
	query := fmt.Sprintf("DELETE FROM %s WHERE endpoint = ? AND id <> ?", machinesTable)
	return s.exec(ctx, query, endpoint, id)
}

func (s Store) DeleteMachine(ctx context.Context, machineID string) error {
	query := fmt.Sprintf("DELETE FROM %s WHERE id = ? OR endpoint = ?", machinesTable)
	return s.exec(ctx, query, machineID, machineID)
}

func (s Store) ListMachineRows(ctx context.Context) ([]mesh.MachineRow, error) {
	query := fmt.Sprintf("SELECT id, public_key, subnet, management_ip, endpoint, updated_at, version FROM %s ORDER BY id", machinesTable)
	rows, err := s.query(ctx, query)
	if err != nil {
		return nil, err
	}
	out := make([]mesh.MachineRow, 0, len(rows))
	for _, row := range rows {
		r, rErr := decodeMachineRow(row)
		if rErr != nil {
			return nil, rErr
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

func (s Store) machineByID(ctx context.Context, id string) (mesh.MachineRow, bool, error) {
	query := fmt.Sprintf(
		"SELECT id, public_key, subnet, management_ip, endpoint, updated_at, version FROM %s WHERE id = ?",
		machinesTable,
	)
	rows, err := s.query(ctx, query, id)
	if err != nil {
		return mesh.MachineRow{}, false, err
	}
	if len(rows) == 0 {
		return mesh.MachineRow{}, false, nil
	}
	out, err := decodeMachineRow(rows[0])
	if err != nil {
		return mesh.MachineRow{}, false, err
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

func (s Store) EnsureHeartbeatTable(ctx context.Context) error {
	query := fmt.Sprintf(`
CREATE TABLE IF NOT EXISTS %s (
    node_id TEXT NOT NULL PRIMARY KEY,
    seq INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL DEFAULT ''
)`, heartbeatsTable)
	return s.exec(ctx, query)
}

func (s Store) BumpHeartbeat(ctx context.Context, nodeID, updatedAt string) error {
	query := fmt.Sprintf(
		`INSERT INTO %s (node_id, seq, updated_at) VALUES (?, 1, ?)
		 ON CONFLICT(node_id) DO UPDATE SET seq = seq + 1, updated_at = ?`,
		heartbeatsTable,
	)
	return s.exec(ctx, query, nodeID, updatedAt, updatedAt)
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

func decodeMachineRow(values []json.RawMessage) (mesh.MachineRow, error) {
	if len(values) != 6 && len(values) != 7 {
		return mesh.MachineRow{}, fmt.Errorf("decode machine row")
	}

	var out mesh.MachineRow
	var err error
	if out.ID, err = decodeString(values[0], "machine id"); err != nil {
		return mesh.MachineRow{}, err
	}
	if out.PublicKey, err = decodeString(values[1], "machine public key"); err != nil {
		return mesh.MachineRow{}, err
	}
	if out.Subnet, err = decodeString(values[2], "machine subnet"); err != nil {
		return mesh.MachineRow{}, err
	}
	if out.Management, err = decodeString(values[3], "machine management ip"); err != nil {
		return mesh.MachineRow{}, err
	}
	if out.Endpoint, err = decodeString(values[4], "machine endpoint"); err != nil {
		return mesh.MachineRow{}, err
	}
	if out.UpdatedAt, err = decodeString(values[5], "machine updated_at"); err != nil {
		return mesh.MachineRow{}, err
	}
	if len(values) == 7 {
		if out.Version, err = decodeInt64(values[6], "machine version"); err != nil {
			return mesh.MachineRow{}, err
		}
	}
	if out.Version <= 0 {
		out.Version = 1
	}
	return out, nil
}

func decodeHeartbeatRow(values []json.RawMessage) (mesh.HeartbeatRow, error) {
	if len(values) != 3 {
		return mesh.HeartbeatRow{}, fmt.Errorf("decode heartbeat row: expected 3 columns, got %d", len(values))
	}
	var out mesh.HeartbeatRow
	var err error
	if out.NodeID, err = decodeString(values[0], "heartbeat node_id"); err != nil {
		return mesh.HeartbeatRow{}, err
	}
	if out.Seq, err = decodeInt64(values[1], "heartbeat seq"); err != nil {
		return mesh.HeartbeatRow{}, err
	}
	if out.UpdatedAt, err = decodeString(values[2], "heartbeat updated_at"); err != nil {
		return mesh.HeartbeatRow{}, err
	}
	return out, nil
}
