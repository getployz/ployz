package store

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"net/netip"
	"time"

	"ployz"
	"ployz/infra/corrosion"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

// machineEventBufCap is the buffer size for the machine event channel.
const machineEventBufCap = 64

const machinesQuery = `SELECT id, name, public_key, endpoints, overlay_ip, labels, updated_at FROM machines`

// ListMachines returns all machine records from the cluster store.
func (s *Store) ListMachines(ctx context.Context) ([]ployz.MachineRecord, error) {
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
func (s *Store) SubscribeMachines(ctx context.Context) ([]ployz.MachineRecord, <-chan ployz.MachineEvent, error) {
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

	events := make(chan ployz.MachineEvent, machineEventBufCap)
	go func() {
		defer close(events)
		defer sub.Close()

		// Track rowID → machine ID so we can resolve deletes.
		// Seeded from insert/update events; initial rows don't carry rowIDs.
		rowIndex := make(map[uint64]string)

		for change := range changes {
			var event ployz.MachineEvent

			switch change.Type {
			case corrosion.ChangeDelete:
				machineID, ok := rowIndex[change.RowID]
				if !ok {
					slog.Warn("Delete for unknown rowID, skipping.", "rowid", change.RowID)
					continue
				}
				delete(rowIndex, change.RowID)
				event = ployz.MachineEvent{
					Kind:   ployz.MachineRemoved,
					Record: ployz.MachineRecord{ID: machineID},
				}
			default:
				var err error
				event, err = changeToMachineEvent(change)
				if err != nil {
					slog.Warn("Skipping malformed machine change event.", "err", err)
					continue
				}
				rowIndex[change.RowID] = event.Record.ID
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
func (s *Store) UpsertMachine(ctx context.Context, rec ployz.MachineRecord) error {
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
func (s *Store) DeleteMachine(ctx context.Context, id string) error {
	_, err := s.client.ExecContext(ctx, `DELETE FROM machines WHERE id = ?`, id)
	if err != nil {
		return fmt.Errorf("delete machine %s: %w", id, err)
	}
	return nil
}

func scanMachineRecord(rows *corrosion.Rows) (ployz.MachineRecord, error) {
	var (
		id, name, pubKeyStr, epJSON, overlayStr, labelsJSON, updatedStr string
	)
	if err := rows.Scan(&id, &name, &pubKeyStr, &epJSON, &overlayStr, &labelsJSON, &updatedStr); err != nil {
		return ployz.MachineRecord{}, fmt.Errorf("scan machine row: %w", err)
	}
	return parseMachineFields(id, name, pubKeyStr, epJSON, overlayStr, labelsJSON, updatedStr)
}

// changeToMachineEvent parses an insert or update change into a MachineEvent.
// Deletes are handled by the caller using the rowID index.
func changeToMachineEvent(change *corrosion.ChangeEvent) (ployz.MachineEvent, error) {
	var kind ployz.MachineEventKind
	switch change.Type {
	case corrosion.ChangeInsert:
		kind = ployz.MachineAdded
	case corrosion.ChangeUpdate:
		kind = ployz.MachineUpdated
	default:
		return ployz.MachineEvent{}, fmt.Errorf("unknown change type: %s", change.Type)
	}

	var (
		id, name, pubKeyStr, epJSON, overlayStr, labelsJSON, updatedStr string
	)
	if err := change.Scan(&id, &name, &pubKeyStr, &epJSON, &overlayStr, &labelsJSON, &updatedStr); err != nil {
		return ployz.MachineEvent{}, fmt.Errorf("scan change event: %w", err)
	}

	rec, err := parseMachineFields(id, name, pubKeyStr, epJSON, overlayStr, labelsJSON, updatedStr)
	if err != nil {
		return ployz.MachineEvent{}, err
	}
	return ployz.MachineEvent{Kind: kind, Record: rec}, nil
}

// parseMachineFields converts raw column strings into a MachineRecord.
func parseMachineFields(id, name, pubKeyStr, epJSON, overlayStr, labelsJSON, updatedStr string) (ployz.MachineRecord, error) {
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
		ap, err := netip.ParseAddrPort(s)
		if err != nil {
			return nil, fmt.Errorf("parse endpoint %q: %w", s, err)
		}
		addrs[i] = ap
	}
	return addrs, nil
}
