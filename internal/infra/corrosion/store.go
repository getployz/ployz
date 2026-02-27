package corrosion

import (
	"context"
	"encoding/json"
	"fmt"
	"net/netip"
	"strconv"
	"strings"

	"ployz/internal/daemon/overlay"
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
	baseURL  string // pre-computed "http://<addr>" to avoid repeated string concat on hot path
	client   corrosionClient
}

func NewStore(apiAddr netip.AddrPort, apiToken string) Store {
	token := strings.TrimSpace(apiToken)
	baseURL := "http://" + apiAddr.String()
	return Store{
		apiAddr:  apiAddr,
		apiToken: token,
		baseURL:  baseURL,
		client:   newHTTPCorrosionClient(baseURL, token),
	}
}

func (s Store) clientOrDefault() corrosionClient {
	if s.client != nil {
		return s.client
	}
	return newHTTPCorrosionClient(s.baseURL, s.apiToken)
}

func (s Store) exec(ctx context.Context, query string, args ...any) error {
	return s.clientOrDefault().exec(ctx, query, args...)
}

func (s Store) query(ctx context.Context, query string, args ...any) ([][]json.RawMessage, error) {
	return s.clientOrDefault().query(ctx, query, args...)
}

func (s Store) subscribe(ctx context.Context, query string, args []any) (*subscriptionStream, error) {
	return s.clientOrDefault().subscribe(ctx, query, args)
}

func (s Store) resubscribe(ctx context.Context, id string, fromChange uint64) (*subscriptionStream, error) {
	return s.clientOrDefault().resubscribe(ctx, id, fromChange)
}

func (s Store) EnsureMachineTable(ctx context.Context) error {
	return s.Machines().EnsureMachineTable(ctx)
}

func (s Store) EnsureNetworkConfigTable(ctx context.Context) error {
	return s.NetworkConfig().EnsureNetworkConfigTable(ctx)
}

func (s Store) EnsureNetworkCIDR(
	ctx context.Context,
	requested netip.Prefix,
	fallbackCIDR string,
	defaultCIDR netip.Prefix,
) (netip.Prefix, error) {
	return s.NetworkConfig().EnsureNetworkCIDR(ctx, requested, fallbackCIDR, defaultCIDR)
}

func (s Store) UpsertMachine(ctx context.Context, row overlay.MachineRow, expectedVersion int64) error {
	return s.Machines().UpsertMachine(ctx, row, expectedVersion)
}

func (s Store) DeleteByEndpointExceptID(ctx context.Context, endpoint, id string) error {
	return s.Machines().DeleteByEndpointExceptID(ctx, endpoint, id)
}

func (s Store) DeleteMachine(ctx context.Context, machineID string) error {
	return s.Machines().DeleteMachine(ctx, machineID)
}

func (s Store) ListMachineRows(ctx context.Context) ([]overlay.MachineRow, error) {
	return s.Machines().ListMachineRows(ctx)
}

func decodeString(raw json.RawMessage, label string) (string, error) {
	// Try nullable string first to handle both "value" and null in one pass.
	var p *string
	if err := json.Unmarshal(raw, &p); err != nil {
		return "", fmt.Errorf("decode %s: %w", label, err)
	}
	if p == nil {
		return "", nil
	}
	return *p, nil
}

func (s Store) EnsureHeartbeatTable(ctx context.Context) error {
	return s.Heartbeats().EnsureHeartbeatTable(ctx)
}

func (s Store) BumpHeartbeat(ctx context.Context, nodeID, updatedAt string) error {
	return s.Heartbeats().BumpHeartbeat(ctx, nodeID, updatedAt)
}

func decodeInt64(raw json.RawMessage, label string) (int64, error) {
	var n int64
	if err := json.Unmarshal(raw, &n); err == nil {
		return n, nil
	}
	// Corrosion may return numbers as floats or strings; try both fallbacks.
	var f float64
	if err := json.Unmarshal(raw, &f); err == nil {
		return int64(f), nil
	}
	var s string
	if err := json.Unmarshal(raw, &s); err == nil {
		if i, convErr := strconv.ParseInt(strings.TrimSpace(s), 10, 64); convErr == nil {
			return i, nil
		}
	}
	return 0, fmt.Errorf("decode %s: %s", label, string(raw))
}
