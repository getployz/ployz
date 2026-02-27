package corrosion

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"strings"
	"time"

	"ployz/internal/daemon/overlay"
)

const (
	changeBufCapacity      = 128
	maxResubscribeBackoff  = 15 * time.Second
	maxResubscribeAttempts = 20
)

func (s Store) SubscribeMachines(ctx context.Context) ([]overlay.MachineRow, <-chan overlay.MachineChange, error) {
	return s.Machines().SubscribeMachines(ctx)
}

func (s Store) SubscribeHeartbeats(ctx context.Context) ([]overlay.HeartbeatRow, <-chan overlay.HeartbeatChange, error) {
	return s.Heartbeats().SubscribeHeartbeats(ctx)
}

// parseChangeKind maps Corrosion change type strings to overlay.ChangeKind values.
func parseChangeKind(typ string) overlay.ChangeKind {
	switch strings.ToLower(strings.TrimSpace(typ)) {
	case "insert":
		return overlay.ChangeAdded
	case "delete":
		return overlay.ChangeDeleted
	default:
		return overlay.ChangeUpdated
	}
}

// subscriptionSpec parameterizes the generic subscription loop for a specific row/change type.
type subscriptionSpec[Row any, Change any] struct {
	label      string
	decodeRow  func([]json.RawMessage) (Row, error)
	makeChange func(overlay.ChangeKind, Row) Change
	resyncMsg  Change
}

var machineSpec = subscriptionSpec[overlay.MachineRow, overlay.MachineChange]{
	label:     "machine",
	decodeRow: decodeMachineRow,
	makeChange: func(kind overlay.ChangeKind, row overlay.MachineRow) overlay.MachineChange {
		return overlay.MachineChange{Kind: kind, Machine: row}
	},
	resyncMsg: overlay.MachineChange{Kind: overlay.ChangeResync},
}

var heartbeatSpec = subscriptionSpec[overlay.HeartbeatRow, overlay.HeartbeatChange]{
	label:     "heartbeat",
	decodeRow: decodeHeartbeatRow,
	makeChange: func(kind overlay.ChangeKind, row overlay.HeartbeatRow) overlay.HeartbeatChange {
		return overlay.HeartbeatChange{Kind: kind, Heartbeat: row}
	},
	resyncMsg: overlay.HeartbeatChange{Kind: overlay.ChangeResync},
}

func openAndRun[Row any, Change any](
	ctx context.Context,
	c corrosionClient,
	query string,
	args []any,
	spec subscriptionSpec[Row, Change],
) ([]Row, <-chan Change, error) {
	stream, snapshot, lastChangeID, err := openSubscription(ctx, c, query, args, spec)
	if err != nil {
		return nil, nil, err
	}
	slog.Debug("registry "+spec.label+" subscription opened", "rows", len(snapshot), "change_id", lastChangeID)

	changes := make(chan Change, changeBufCapacity)
	go runChanges(ctx, c, stream, lastChangeID, changes, spec)
	return snapshot, changes, nil
}

func openSubscription[Row any, Change any](
	ctx context.Context,
	c corrosionClient,
	query string,
	args []any,
	spec subscriptionSpec[Row, Change],
) (*subscriptionStream, []Row, uint64, error) {
	stream, err := c.subscribe(ctx, query, args)
	if err != nil {
		return nil, nil, 0, err
	}

	var ev queryEvent
	if err := stream.Decoder.Decode(&ev); err != nil {
		stream.Body.Close()
		return nil, nil, 0, fmt.Errorf("decode %s subscription columns: %w", spec.label, err)
	}
	if ev.Error != nil {
		stream.Body.Close()
		return nil, nil, 0, fmt.Errorf("%s subscription error: %s", spec.label, *ev.Error)
	}

	var snapshot []Row
	var lastChange uint64
	for {
		ev = queryEvent{}
		if err := stream.Decoder.Decode(&ev); err != nil {
			stream.Body.Close()
			return nil, nil, 0, fmt.Errorf("decode %s subscription row: %w", spec.label, err)
		}
		if ev.Error != nil {
			stream.Body.Close()
			return nil, nil, 0, fmt.Errorf("%s subscription error: %s", spec.label, *ev.Error)
		}
		if ev.Row != nil {
			row, rowErr := spec.decodeRow(ev.Row.Values)
			if rowErr != nil {
				stream.Body.Close()
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

func runChanges[Row any, Change any](
	ctx context.Context,
	c corrosionClient,
	stream *subscriptionStream,
	lastChangeID uint64,
	out chan<- Change,
	spec subscriptionSpec[Row, Change],
) {
	defer close(out)
	defer stream.Body.Close()

	for {
		select {
		case <-ctx.Done():
			slog.Debug("registry "+spec.label+" subscription closed", "reason", "context_cancelled")
			return
		default:
		}

		var ev queryEvent
		if err := stream.Decoder.Decode(&ev); err != nil {
			slog.Debug("registry "+spec.label+" subscription decode failed; resubscribing", "err", err)
			if !resubscribeLoop(ctx, c, stream, &lastChangeID, out, spec) {
				return
			}
			continue
		}
		if ev.Error != nil {
			slog.Debug("registry "+spec.label+" subscription stream error; resubscribing", "err", *ev.Error)
			if !resubscribeLoop(ctx, c, stream, &lastChangeID, out, spec) {
				return
			}
			continue
		}
		if ev.Change == nil {
			continue
		}

		row, err := spec.decodeRow(ev.Change.Values)
		if err != nil {
			slog.Debug("registry "+spec.label+" change decode failed; resubscribing", "err", err)
			if !resubscribeLoop(ctx, c, stream, &lastChangeID, out, spec) {
				return
			}
			continue
		}

		lastChangeID = ev.Change.ChangeID

		select {
		case <-ctx.Done():
			return
		case out <- spec.makeChange(parseChangeKind(ev.Change.Type), row):
		}
	}
}

func resubscribeLoop[Row any, Change any](
	ctx context.Context,
	c corrosionClient,
	stream *subscriptionStream,
	lastChangeID *uint64,
	out chan<- Change,
	spec subscriptionSpec[Row, Change],
) bool {
	stream.Body.Close()

	backoff := time.Second
	for attempt := range maxResubscribeAttempts {
		select {
		case <-ctx.Done():
			slog.Debug("registry "+spec.label+" subscription closed", "reason", "context_cancelled")
			return false
		case <-time.After(backoff):
		}

		next, err := c.resubscribe(ctx, stream.ID, *lastChangeID)
		if err == nil {
			stream.Body = next.Body
			stream.Decoder = next.Decoder
			slog.Info("registry "+spec.label+" subscription restored", "change_id", *lastChangeID)
			select {
			case <-ctx.Done():
				slog.Debug("registry "+spec.label+" subscription closed", "reason", "context_cancelled")
				stream.Body.Close()
				return false
			case out <- spec.resyncMsg:
			}
			return true
		}

		slog.Debug("registry "+spec.label+" resubscribe failed", "change_id", *lastChangeID, "attempt", attempt+1, "backoff", backoff.String(), "err", err)
		backoff = min(backoff*2, maxResubscribeBackoff)
	}
	slog.Warn("registry "+spec.label+" resubscribe exhausted retries", "change_id", *lastChangeID, "attempts", maxResubscribeAttempts)
	return false
}
