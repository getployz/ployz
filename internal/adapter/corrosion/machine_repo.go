package corrosion

import (
	"context"
	"encoding/json"
	"fmt"
	"net/netip"
	"strings"

	"ployz/internal/network"
	"ployz/internal/supervisor"
)

var (
	_ network.MachineRegistry    = MachineRepo{}
	_ supervisor.MachineRegistry = MachineRepo{}
)

// MachineRepo provides machine-table operations over Corrosion.
type MachineRepo struct {
	client corrosionClient
}

// NewMachineRepo creates a machine repository from Corrosion API coordinates.
func NewMachineRepo(apiAddr netip.AddrPort, apiToken string) MachineRepo {
	return NewStore(apiAddr, apiToken).Machines()
}

// Machines returns a machine-scoped repository backed by this Store.
func (s Store) Machines() MachineRepo {
	return MachineRepo{client: s.clientOrDefault()}
}

func (r MachineRepo) EnsureMachineTable(ctx context.Context) error {
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
	return r.client.exec(ctx, query)
}

func (r MachineRepo) UpsertMachine(ctx context.Context, row network.MachineRow, expectedVersion int64) error {
	row.ID = strings.TrimSpace(row.ID)
	if row.ID == "" {
		return fmt.Errorf("machine id is required")
	}

	current, exists, err := r.machineByID(ctx, row.ID)
	if err != nil {
		return err
	}

	if exists {
		if expectedVersion > 0 && current.Version != expectedVersion {
			return network.ErrConflict
		}
		if current.PublicKey == row.PublicKey &&
			current.Subnet == row.Subnet &&
			current.ManagementIP == row.ManagementIP &&
			current.Endpoint == row.Endpoint {
			return nil
		}
		row.Version = current.Version + 1
		query := fmt.Sprintf(
			"UPDATE %s SET public_key = ?, subnet = ?, management_ip = ?, endpoint = ?, updated_at = ?, version = ? WHERE id = ?",
			machinesTable,
		)
		return r.client.exec(ctx, query, row.PublicKey, row.Subnet, row.ManagementIP, row.Endpoint, row.UpdatedAt, row.Version, row.ID)
	}

	if expectedVersion > 0 {
		return network.ErrConflict
	}
	if row.Version <= 0 {
		row.Version = 1
	}
	query := fmt.Sprintf(
		"INSERT INTO %s (id, public_key, subnet, management_ip, endpoint, updated_at, version) VALUES (?, ?, ?, ?, ?, ?, ?)",
		machinesTable,
	)
	return r.client.exec(ctx, query, row.ID, row.PublicKey, row.Subnet, row.ManagementIP, row.Endpoint, row.UpdatedAt, row.Version)
}

func (r MachineRepo) DeleteByEndpointExceptID(ctx context.Context, endpoint, id string) error {
	query := fmt.Sprintf("DELETE FROM %s WHERE endpoint = ? AND id <> ?", machinesTable)
	return r.client.exec(ctx, query, endpoint, id)
}

func (r MachineRepo) DeleteMachine(ctx context.Context, machineID string) error {
	query := fmt.Sprintf("DELETE FROM %s WHERE id = ? OR endpoint = ?", machinesTable)
	return r.client.exec(ctx, query, machineID, machineID)
}

func (r MachineRepo) ListMachineRows(ctx context.Context) ([]network.MachineRow, error) {
	query := fmt.Sprintf("SELECT id, public_key, subnet, management_ip, endpoint, updated_at, version FROM %s ORDER BY id", machinesTable)
	rows, err := r.client.query(ctx, query)
	if err != nil {
		return nil, err
	}
	out := make([]network.MachineRow, 0, len(rows))
	for _, row := range rows {
		decoded, decodeErr := decodeMachineRow(row)
		if decodeErr != nil {
			return nil, decodeErr
		}
		out = append(out, decoded)
	}
	return out, nil
}

func (r MachineRepo) SubscribeMachines(ctx context.Context) ([]network.MachineRow, <-chan network.MachineChange, error) {
	query := fmt.Sprintf("SELECT id, public_key, subnet, management_ip, endpoint, updated_at, version FROM %s ORDER BY id", machinesTable)
	return openAndRun(ctx, r.client, query, nil, machineSpec)
}

func (r MachineRepo) machineByID(ctx context.Context, id string) (network.MachineRow, bool, error) {
	query := fmt.Sprintf(
		"SELECT id, public_key, subnet, management_ip, endpoint, updated_at, version FROM %s WHERE id = ?",
		machinesTable,
	)
	rows, err := r.client.query(ctx, query, id)
	if err != nil {
		return network.MachineRow{}, false, err
	}
	if len(rows) == 0 {
		return network.MachineRow{}, false, nil
	}
	out, err := decodeMachineRow(rows[0])
	if err != nil {
		return network.MachineRow{}, false, err
	}
	return out, true, nil
}

func decodeMachineRow(values []json.RawMessage) (network.MachineRow, error) {
	if len(values) != 6 && len(values) != 7 {
		return network.MachineRow{}, fmt.Errorf("decode machine row: expected 6 or 7 columns, got %d", len(values))
	}

	var out network.MachineRow
	var err error
	if out.ID, err = decodeString(values[0], "machine id"); err != nil {
		return network.MachineRow{}, err
	}
	if out.PublicKey, err = decodeString(values[1], "machine public key"); err != nil {
		return network.MachineRow{}, err
	}
	if out.Subnet, err = decodeString(values[2], "machine subnet"); err != nil {
		return network.MachineRow{}, err
	}
	if out.ManagementIP, err = decodeString(values[3], "machine management ip"); err != nil {
		return network.MachineRow{}, err
	}
	if out.Endpoint, err = decodeString(values[4], "machine endpoint"); err != nil {
		return network.MachineRow{}, err
	}
	if out.UpdatedAt, err = decodeString(values[5], "machine updated_at"); err != nil {
		return network.MachineRow{}, err
	}
	if len(values) == 7 {
		if out.Version, err = decodeInt64(values[6], "machine version"); err != nil {
			return network.MachineRow{}, err
		}
	}
	if out.Version <= 0 {
		out.Version = 1
	}
	return out, nil
}
