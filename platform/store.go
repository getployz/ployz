package platform

import (
	"context"
	"encoding/json"
	"fmt"
	"net/netip"
	"time"

	"ployz"
	"ployz/platform/corrosion"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

// ClusterStore implements machine.ClusterStore backed by Corrosion.
type ClusterStore struct {
	client *corrosion.Client
}

// NewClusterStore creates a ClusterStore using the given Corrosion client.
func NewClusterStore(client *corrosion.Client) *ClusterStore {
	return &ClusterStore{client: client}
}

const machinesQuery = `SELECT id, name, public_key, endpoints, overlay_ip, labels, updated_at FROM machines`

// ListMachines returns all machine records from the cluster store.
func (s *ClusterStore) ListMachines(ctx context.Context) ([]ployz.MachineRecord, error) {
	rows, err := s.client.QueryContext(ctx, machinesQuery)
	if err != nil {
		return nil, fmt.Errorf("list machines: %w", err)
	}
	defer rows.Close()

	var records []ployz.MachineRecord
	for rows.Next() {
		rec, err := scanMachineRecord(rows)
		if err != nil {
			return nil, fmt.Errorf("list machines: %w", err)
		}
		records = append(records, rec)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("list machines: %w", err)
	}
	return records, nil
}

// SubscribeMachines returns the current machine set and a channel of change events.
func (s *ClusterStore) SubscribeMachines(ctx context.Context) ([]ployz.MachineRecord, <-chan ployz.MachineEvent, error) {
	sub, err := s.client.SubscribeContext(ctx, machinesQuery, nil, false)
	if err != nil {
		return nil, nil, fmt.Errorf("subscribe machines: %w", err)
	}

	// Consume initial rows.
	initialRows := sub.Rows()
	var records []ployz.MachineRecord
	if initialRows != nil {
		for initialRows.Next() {
			rec, err := scanMachineRecord(initialRows)
			if err != nil {
				sub.Close()
				return nil, nil, fmt.Errorf("subscribe machines initial scan: %w", err)
			}
			records = append(records, rec)
		}
		if err := initialRows.Err(); err != nil {
			sub.Close()
			return nil, nil, fmt.Errorf("subscribe machines initial rows: %w", err)
		}
	}

	changes, err := sub.Changes()
	if err != nil {
		sub.Close()
		return nil, nil, fmt.Errorf("subscribe machines changes: %w", err)
	}

	events := make(chan ployz.MachineEvent, 64)
	go func() {
		defer close(events)
		defer sub.Close()

		for change := range changes {
			event, err := changeToMachineEvent(change)
			if err != nil {
				// TODO: log and continue, or expose error channel
				continue
			}
			select {
			case events <- event:
			case <-ctx.Done():
				return
			}
		}
	}()

	return records, events, nil
}

// UpsertMachine inserts or updates a machine record.
func (s *ClusterStore) UpsertMachine(ctx context.Context, rec ployz.MachineRecord) error {
	endpoints, err := json.Marshal(addrPortsToStrings(rec.Endpoints))
	if err != nil {
		return fmt.Errorf("upsert machine: marshal endpoints: %w", err)
	}
	labels, err := json.Marshal(rec.Labels)
	if err != nil {
		return fmt.Errorf("upsert machine: marshal labels: %w", err)
	}

	_, err = s.client.ExecContext(ctx,
		`INSERT OR REPLACE INTO machines (id, name, public_key, endpoints, overlay_ip, labels, updated_at)
		 VALUES (?, ?, ?, ?, ?, ?, ?)`,
		rec.ID,
		rec.Name,
		rec.PublicKey.String(),
		string(endpoints),
		rec.OverlayIP.String(),
		string(labels),
		rec.UpdatedAt.UTC().Format(time.RFC3339),
	)
	if err != nil {
		return fmt.Errorf("upsert machine %s: %w", rec.ID, err)
	}
	return nil
}

// DeleteMachine removes a machine record by ID.
func (s *ClusterStore) DeleteMachine(ctx context.Context, id string) error {
	_, err := s.client.ExecContext(ctx, `DELETE FROM machines WHERE id = ?`, id)
	if err != nil {
		return fmt.Errorf("delete machine %s: %w", id, err)
	}
	return nil
}

// scanMachineRecord reads a MachineRecord from a Rows cursor.
func scanMachineRecord(rows *corrosion.Rows) (ployz.MachineRecord, error) {
	var (
		id         string
		name       string
		pubKeyStr  string
		epJSON     string
		overlayStr string
		labelsJSON string
		updatedStr string
	)
	if err := rows.Scan(&id, &name, &pubKeyStr, &epJSON, &overlayStr, &labelsJSON, &updatedStr); err != nil {
		return ployz.MachineRecord{}, fmt.Errorf("scan machine row: %w", err)
	}

	pubKey, err := wgtypes.ParseKey(pubKeyStr)
	if err != nil {
		return ployz.MachineRecord{}, fmt.Errorf("parse public key for %s: %w", id, err)
	}

	endpoints, err := parseAddrPorts(epJSON)
	if err != nil {
		return ployz.MachineRecord{}, fmt.Errorf("parse endpoints for %s: %w", id, err)
	}

	overlayIP, err := netip.ParseAddr(overlayStr)
	if err != nil {
		return ployz.MachineRecord{}, fmt.Errorf("parse overlay IP for %s: %w", id, err)
	}

	var labels map[string]string
	if err := json.Unmarshal([]byte(labelsJSON), &labels); err != nil {
		return ployz.MachineRecord{}, fmt.Errorf("parse labels for %s: %w", id, err)
	}

	updatedAt, err := time.Parse(time.RFC3339, updatedStr)
	if err != nil {
		return ployz.MachineRecord{}, fmt.Errorf("parse updated_at for %s: %w", id, err)
	}

	return ployz.MachineRecord{
		ID:        id,
		Name:      name,
		PublicKey: pubKey,
		Endpoints: endpoints,
		OverlayIP: overlayIP,
		Labels:    labels,
		UpdatedAt: updatedAt,
	}, nil
}

func changeToMachineEvent(change *corrosion.ChangeEvent) (ployz.MachineEvent, error) {
	var kind ployz.MachineEventKind
	switch change.Type {
	case corrosion.ChangeInsert:
		kind = ployz.MachineAdded
	case corrosion.ChangeUpdate:
		kind = ployz.MachineUpdated
	case corrosion.ChangeDelete:
		kind = ployz.MachineRemoved
	default:
		return ployz.MachineEvent{}, fmt.Errorf("unknown change type: %s", change.Type)
	}

	// For deletes, we only have the row ID — the record will be sparse.
	if change.Type == corrosion.ChangeDelete {
		return ployz.MachineEvent{Kind: kind}, nil
	}

	var (
		id         string
		name       string
		pubKeyStr  string
		epJSON     string
		overlayStr string
		labelsJSON string
		updatedStr string
	)
	if err := change.Scan(&id, &name, &pubKeyStr, &epJSON, &overlayStr, &labelsJSON, &updatedStr); err != nil {
		return ployz.MachineEvent{}, fmt.Errorf("scan change event: %w", err)
	}

	pubKey, err := wgtypes.ParseKey(pubKeyStr)
	if err != nil {
		return ployz.MachineEvent{}, fmt.Errorf("parse public key in change: %w", err)
	}
	endpoints, err := parseAddrPorts(epJSON)
	if err != nil {
		return ployz.MachineEvent{}, fmt.Errorf("parse endpoints in change: %w", err)
	}
	overlayIP, err := netip.ParseAddr(overlayStr)
	if err != nil {
		return ployz.MachineEvent{}, fmt.Errorf("parse overlay IP in change: %w", err)
	}
	var labels map[string]string
	if err := json.Unmarshal([]byte(labelsJSON), &labels); err != nil {
		return ployz.MachineEvent{}, fmt.Errorf("parse labels in change: %w", err)
	}
	updatedAt, _ := time.Parse(time.RFC3339, updatedStr) // best-effort; change events may have different format

	return ployz.MachineEvent{
		Kind: kind,
		Record: ployz.MachineRecord{
			ID:        id,
			Name:      name,
			PublicKey: pubKey,
			Endpoints: endpoints,
			OverlayIP: overlayIP,
			Labels:    labels,
			UpdatedAt: updatedAt,
		},
	}, nil
}

func addrPortsToStrings(addrs []netip.AddrPort) []string {
	out := make([]string, len(addrs))
	for i, a := range addrs {
		out[i] = a.String()
	}
	return out
}

func parseAddrPorts(jsonStr string) ([]netip.AddrPort, error) {
	var strs []string
	if err := json.Unmarshal([]byte(jsonStr), &strs); err != nil {
		return nil, fmt.Errorf("unmarshal endpoints: %w", err)
	}
	addrs := make([]netip.AddrPort, len(strs))
	for i, s := range strs {
		var err error
		addrs[i], err = netip.ParseAddrPort(s)
		if err != nil {
			return nil, fmt.Errorf("parse endpoint %q: %w", s, err)
		}
	}
	return addrs, nil
}
