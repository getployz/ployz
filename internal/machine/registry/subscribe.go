package registry

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"time"
)

type ChangeKind int

const (
	ChangeAdded ChangeKind = iota
	ChangeUpdated
	ChangeDeleted
	ChangeResync
)

type MachineChange struct {
	Kind    ChangeKind
	Machine MachineRow
}

func (s Store) SubscribeMachines(ctx context.Context) ([]MachineRow, <-chan MachineChange, error) {
	query := fmt.Sprintf("SELECT id, public_key, subnet, management_ip, endpoint, updated_at, version FROM %s ORDER BY id", machinesTable)
	stream, snapshot, lastChangeID, err := s.openMachinesSubscription(ctx, query)
	if err != nil {
		return nil, nil, err
	}

	changes := make(chan MachineChange, 128)
	go s.runMachineChanges(ctx, stream, lastChangeID, changes)
	return snapshot, changes, nil
}

func (s Store) openMachinesSubscription(
	ctx context.Context,
	query string,
) (*subscriptionStream, []MachineRow, uint64, error) {
	stream, err := s.subscribe(ctx, query, nil)
	if err != nil {
		return nil, nil, 0, err
	}

	var ev queryEvent
	if err := stream.Decoder.Decode(&ev); err != nil {
		_ = stream.Body.Close()
		return nil, nil, 0, fmt.Errorf("decode corrosion subscription columns: %w", err)
	}
	if ev.Error != nil {
		_ = stream.Body.Close()
		return nil, nil, 0, fmt.Errorf("corrosion subscription error: %s", *ev.Error)
	}

	snapshot := make([]MachineRow, 0)
	var lastChange uint64
	for {
		ev = queryEvent{}
		if err := stream.Decoder.Decode(&ev); err != nil {
			_ = stream.Body.Close()
			return nil, nil, 0, fmt.Errorf("decode corrosion subscription row: %w", err)
		}
		if ev.Error != nil {
			_ = stream.Body.Close()
			return nil, nil, 0, fmt.Errorf("corrosion subscription error: %s", *ev.Error)
		}
		if ev.Row != nil {
			row, rowErr := decodeMachineRow(ev.Row.Values)
			if rowErr != nil {
				_ = stream.Body.Close()
				return nil, nil, 0, rowErr
			}
			snapshot = append(snapshot, row)
			continue
		}
		if ev.EOQ != nil {
			if ev.EOQ.ChangeID != nil {
				lastChange = *ev.EOQ.ChangeID
			}
			break
		}
	}

	return stream, snapshot, lastChange, nil
}

func (s Store) runMachineChanges(
	ctx context.Context,
	stream *subscriptionStream,
	lastChangeID uint64,
	out chan<- MachineChange,
) {
	defer close(out)
	defer stream.Body.Close()

	for {
		select {
		case <-ctx.Done():
			return
		default:
		}

		var ev queryEvent
		if err := stream.Decoder.Decode(&ev); err != nil {
			if !s.resubscribeMachines(ctx, stream, &lastChangeID, out) {
				return
			}
			continue
		}
		if ev.Error != nil {
			if !s.resubscribeMachines(ctx, stream, &lastChangeID, out) {
				return
			}
			continue
		}
		if ev.Change == nil {
			continue
		}

		row, err := decodeMachineRow(ev.Change.Values)
		if err != nil {
			if !s.resubscribeMachines(ctx, stream, &lastChangeID, out) {
				return
			}
			continue
		}

		kind := ChangeUpdated
		switch strings.ToLower(strings.TrimSpace(ev.Change.Type)) {
		case "insert":
			kind = ChangeAdded
		case "update":
			kind = ChangeUpdated
		case "delete":
			kind = ChangeDeleted
		}
		lastChangeID = ev.Change.ChangeID

		select {
		case <-ctx.Done():
			return
		case out <- MachineChange{Kind: kind, Machine: row}:
		}
	}
}

func (s Store) resubscribeMachines(
	ctx context.Context,
	stream *subscriptionStream,
	lastChangeID *uint64,
	out chan<- MachineChange,
) bool {
	_ = stream.Body.Close()

	backoff := time.Second
	for {
		select {
		case <-ctx.Done():
			return false
		case <-time.After(backoff):
		}

		next, err := s.resubscribe(ctx, stream.ID, *lastChangeID)
		if err == nil {
			stream.Body = next.Body
			stream.Decoder = next.Decoder
			select {
			case <-ctx.Done():
				_ = stream.Body.Close()
				return false
			case out <- MachineChange{Kind: ChangeResync}:
			}
			return true
		}

		if backoff < 15*time.Second {
			backoff *= 2
			if backoff > 15*time.Second {
				backoff = 15 * time.Second
			}
		}
	}
}

func decodeMachineRow(values []json.RawMessage) (MachineRow, error) {
	if len(values) != 6 && len(values) != 7 {
		return MachineRow{}, fmt.Errorf("decode machine row")
	}

	var out MachineRow
	var err error
	if out.ID, err = decodeString(values[0], "machine id"); err != nil {
		return MachineRow{}, err
	}
	if out.PublicKey, err = decodeString(values[1], "machine public key"); err != nil {
		return MachineRow{}, err
	}
	if out.Subnet, err = decodeString(values[2], "machine subnet"); err != nil {
		return MachineRow{}, err
	}
	if out.Management, err = decodeString(values[3], "machine management ip"); err != nil {
		return MachineRow{}, err
	}
	if out.Endpoint, err = decodeString(values[4], "machine endpoint"); err != nil {
		return MachineRow{}, err
	}
	if out.UpdatedAt, err = decodeString(values[5], "machine updated_at"); err != nil {
		return MachineRow{}, err
	}
	if len(values) == 7 {
		if out.Version, err = decodeInt64(values[6], "machine version"); err != nil {
			return MachineRow{}, err
		}
	}
	if out.Version <= 0 {
		out.Version = 1
	}
	return out, nil
}
