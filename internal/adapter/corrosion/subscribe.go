package corrosion

import (
	"context"
	"fmt"
	"log/slog"
	"strings"
	"time"

	"ployz/internal/network"
)

func (s Store) SubscribeMachines(ctx context.Context) ([]network.MachineRow, <-chan network.MachineChange, error) {
	query := fmt.Sprintf("SELECT id, public_key, subnet, management_ip, endpoint, updated_at, version FROM %s ORDER BY id", machinesTable)
	stream, snapshot, lastChangeID, err := s.openMachinesSubscription(ctx, query)
	if err != nil {
		return nil, nil, err
	}
	slog.Debug("registry machine subscription opened", "rows", len(snapshot), "change_id", lastChangeID)

	changes := make(chan network.MachineChange, 128)
	go s.runMachineChanges(ctx, stream, lastChangeID, changes)
	return snapshot, changes, nil
}

func (s Store) openMachinesSubscription(
	ctx context.Context,
	query string,
) (*subscriptionStream, []network.MachineRow, uint64, error) {
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

	snapshot := make([]network.MachineRow, 0)
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
	out chan<- network.MachineChange,
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
			slog.Debug("registry machine subscription decode failed; resubscribing", "err", err)
			if !s.resubscribeMachines(ctx, stream, &lastChangeID, out) {
				return
			}
			continue
		}
		if ev.Error != nil {
			slog.Debug("registry machine subscription stream error; resubscribing", "err", *ev.Error)
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
			slog.Debug("registry machine change decode failed; resubscribing", "err", err)
			if !s.resubscribeMachines(ctx, stream, &lastChangeID, out) {
				return
			}
			continue
		}

		kind := network.ChangeUpdated
		switch strings.ToLower(strings.TrimSpace(ev.Change.Type)) {
		case "insert":
			kind = network.ChangeAdded
		case "update":
			kind = network.ChangeUpdated
		case "delete":
			kind = network.ChangeDeleted
		}
		lastChangeID = ev.Change.ChangeID

		select {
		case <-ctx.Done():
			return
		case out <- network.MachineChange{Kind: kind, Machine: row}:
		}
	}
}

func (s Store) resubscribeMachines(
	ctx context.Context,
	stream *subscriptionStream,
	lastChangeID *uint64,
	out chan<- network.MachineChange,
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
			slog.Info("registry machine subscription restored", "change_id", *lastChangeID)
			select {
			case <-ctx.Done():
				_ = stream.Body.Close()
				return false
			case out <- network.MachineChange{Kind: network.ChangeResync}:
			}
			return true
		}

		slog.Debug("registry machine resubscribe failed", "change_id", *lastChangeID, "backoff", backoff.String(), "err", err)

		if backoff < 15*time.Second {
			backoff *= 2
			if backoff > 15*time.Second {
				backoff = 15 * time.Second
			}
		}
	}
}

func (s Store) SubscribeHeartbeats(ctx context.Context) ([]network.HeartbeatRow, <-chan network.HeartbeatChange, error) {
	query := fmt.Sprintf("SELECT node_id, seq, updated_at FROM %s ORDER BY node_id", heartbeatsTable)
	stream, snapshot, lastChangeID, err := s.openHeartbeatSubscription(ctx, query)
	if err != nil {
		return nil, nil, err
	}
	slog.Debug("registry heartbeat subscription opened", "rows", len(snapshot), "change_id", lastChangeID)

	changes := make(chan network.HeartbeatChange, 128)
	go s.runHeartbeatChanges(ctx, stream, lastChangeID, changes)
	return snapshot, changes, nil
}

func (s Store) openHeartbeatSubscription(
	ctx context.Context,
	query string,
) (*subscriptionStream, []network.HeartbeatRow, uint64, error) {
	stream, err := s.subscribe(ctx, query, nil)
	if err != nil {
		return nil, nil, 0, err
	}

	var ev queryEvent
	if err := stream.Decoder.Decode(&ev); err != nil {
		_ = stream.Body.Close()
		return nil, nil, 0, fmt.Errorf("decode heartbeat subscription columns: %w", err)
	}
	if ev.Error != nil {
		_ = stream.Body.Close()
		return nil, nil, 0, fmt.Errorf("heartbeat subscription error: %s", *ev.Error)
	}

	snapshot := make([]network.HeartbeatRow, 0)
	var lastChange uint64
	for {
		ev = queryEvent{}
		if err := stream.Decoder.Decode(&ev); err != nil {
			_ = stream.Body.Close()
			return nil, nil, 0, fmt.Errorf("decode heartbeat subscription row: %w", err)
		}
		if ev.Error != nil {
			_ = stream.Body.Close()
			return nil, nil, 0, fmt.Errorf("heartbeat subscription error: %s", *ev.Error)
		}
		if ev.Row != nil {
			row, rowErr := decodeHeartbeatRow(ev.Row.Values)
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

func (s Store) runHeartbeatChanges(
	ctx context.Context,
	stream *subscriptionStream,
	lastChangeID uint64,
	out chan<- network.HeartbeatChange,
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
			slog.Debug("registry heartbeat subscription decode failed; resubscribing", "err", err)
			if !s.resubscribeHeartbeats(ctx, stream, &lastChangeID, out) {
				return
			}
			continue
		}
		if ev.Error != nil {
			slog.Debug("registry heartbeat subscription stream error; resubscribing", "err", *ev.Error)
			if !s.resubscribeHeartbeats(ctx, stream, &lastChangeID, out) {
				return
			}
			continue
		}
		if ev.Change == nil {
			continue
		}

		row, err := decodeHeartbeatRow(ev.Change.Values)
		if err != nil {
			slog.Debug("registry heartbeat change decode failed; resubscribing", "err", err)
			if !s.resubscribeHeartbeats(ctx, stream, &lastChangeID, out) {
				return
			}
			continue
		}

		kind := network.ChangeUpdated
		switch strings.ToLower(strings.TrimSpace(ev.Change.Type)) {
		case "insert":
			kind = network.ChangeAdded
		case "update":
			kind = network.ChangeUpdated
		case "delete":
			kind = network.ChangeDeleted
		}
		lastChangeID = ev.Change.ChangeID

		select {
		case <-ctx.Done():
			return
		case out <- network.HeartbeatChange{Kind: kind, Heartbeat: row}:
		}
	}
}

func (s Store) resubscribeHeartbeats(
	ctx context.Context,
	stream *subscriptionStream,
	lastChangeID *uint64,
	out chan<- network.HeartbeatChange,
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
			slog.Info("registry heartbeat subscription restored", "change_id", *lastChangeID)
			select {
			case <-ctx.Done():
				_ = stream.Body.Close()
				return false
			case out <- network.HeartbeatChange{Kind: network.ChangeResync}:
			}
			return true
		}

		slog.Debug("registry heartbeat resubscribe failed", "change_id", *lastChangeID, "backoff", backoff.String(), "err", err)

		if backoff < 15*time.Second {
			backoff *= 2
			if backoff > 15*time.Second {
				backoff = 15 * time.Second
			}
		}
	}
}
