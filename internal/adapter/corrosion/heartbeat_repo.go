package corrosion

import (
	"context"
	"encoding/json"
	"fmt"
	"net/netip"

	"ployz/internal/network"
	"ployz/internal/supervisor"
)

var _ supervisor.HeartbeatRegistry = HeartbeatRepo{}

// HeartbeatRepo provides heartbeat-table operations over Corrosion.
type HeartbeatRepo struct {
	client corrosionClient
}

// NewHeartbeatRepo creates a heartbeat repository from Corrosion API coordinates.
func NewHeartbeatRepo(apiAddr netip.AddrPort, apiToken string) HeartbeatRepo {
	return NewStore(apiAddr, apiToken).Heartbeats()
}

// Heartbeats returns a heartbeat-scoped repository backed by this Store.
func (s Store) Heartbeats() HeartbeatRepo {
	return HeartbeatRepo{client: s.clientOrDefault()}
}

func (r HeartbeatRepo) EnsureHeartbeatTable(ctx context.Context) error {
	query := fmt.Sprintf(`
CREATE TABLE IF NOT EXISTS %s (
    node_id TEXT NOT NULL PRIMARY KEY,
    seq INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL DEFAULT ''
)`, heartbeatsTable)
	return r.client.exec(ctx, query)
}

func (r HeartbeatRepo) BumpHeartbeat(ctx context.Context, nodeID, updatedAt string) error {
	query := fmt.Sprintf(
		`INSERT INTO %s (node_id, seq, updated_at) VALUES (?, 1, ?)
		 ON CONFLICT(node_id) DO UPDATE SET seq = seq + 1, updated_at = ?`,
		heartbeatsTable,
	)
	return r.client.exec(ctx, query, nodeID, updatedAt, updatedAt)
}

func (r HeartbeatRepo) SubscribeHeartbeats(ctx context.Context) ([]network.HeartbeatRow, <-chan network.HeartbeatChange, error) {
	query := fmt.Sprintf("SELECT node_id, seq, updated_at FROM %s ORDER BY node_id", heartbeatsTable)
	return openAndRun(ctx, r.client, query, nil, heartbeatSpec)
}

func decodeHeartbeatRow(values []json.RawMessage) (network.HeartbeatRow, error) {
	if len(values) != 3 {
		return network.HeartbeatRow{}, fmt.Errorf("decode heartbeat row: expected 3 columns, got %d", len(values))
	}

	var out network.HeartbeatRow
	var err error
	if out.NodeID, err = decodeString(values[0], "heartbeat node_id"); err != nil {
		return network.HeartbeatRow{}, err
	}
	if out.Seq, err = decodeInt64(values[1], "heartbeat seq"); err != nil {
		return network.HeartbeatRow{}, err
	}
	if out.UpdatedAt, err = decodeString(values[2], "heartbeat updated_at"); err != nil {
		return network.HeartbeatRow{}, err
	}
	return out, nil
}
