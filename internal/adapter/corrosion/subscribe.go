package corrosion

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"strings"
	"time"

	"ployz/internal/network"
)

const (
	changeBufCapacity      = 128
	maxResubscribeBackoff  = 15 * time.Second
	maxResubscribeAttempts = 20
)

func (s Store) SubscribeMachines(ctx context.Context) ([]network.MachineRow, <-chan network.MachineChange, error) {
	return s.Machines().SubscribeMachines(ctx)
}

func (s Store) SubscribeHeartbeats(ctx context.Context) ([]network.HeartbeatRow, <-chan network.HeartbeatChange, error) {
	return s.Heartbeats().SubscribeHeartbeats(ctx)
}

// parseChangeKind maps Corrosion change type strings to network.ChangeKind values.
func parseChangeKind(typ string) network.ChangeKind {
	switch strings.ToLower(strings.TrimSpace(typ)) {
	case "insert":
		return network.ChangeAdded
	case "delete":
		return network.ChangeDeleted
	default:
		return network.ChangeUpdated
	}
}

// subscriptionSpec parameterizes the generic subscription loop for a specific row/change type.
type subscriptionSpec[Row any, Change any] struct {
	label      string
	decodeRow  func([]json.RawMessage) (Row, error)
	makeChange func(network.ChangeKind, Row) Change
	resyncMsg  Change
}

var machineSpec = subscriptionSpec[network.MachineRow, network.MachineChange]{
	label:     "machine",
	decodeRow: decodeMachineRow,
	makeChange: func(kind network.ChangeKind, row network.MachineRow) network.MachineChange {
		return network.MachineChange{Kind: kind, Machine: row}
	},
	resyncMsg: network.MachineChange{Kind: network.ChangeResync},
}

var heartbeatSpec = subscriptionSpec[network.HeartbeatRow, network.HeartbeatChange]{
	label:     "heartbeat",
	decodeRow: decodeHeartbeatRow,
	makeChange: func(kind network.ChangeKind, row network.HeartbeatRow) network.HeartbeatChange {
		return network.HeartbeatChange{Kind: kind, Heartbeat: row}
	},
	resyncMsg: network.HeartbeatChange{Kind: network.ChangeResync},
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
	phase := SubscriptionOpening
	phase = phase.Transition(SubscriptionStreaming)

	for {
		select {
		case <-ctx.Done():
			phase = phase.Transition(SubscriptionClosedContext)
			slog.Debug("registry "+spec.label+" subscription closed", "phase", phase.String())
			return
		default:
		}

		var ev queryEvent
		if err := stream.Decoder.Decode(&ev); err != nil {
			slog.Debug("registry "+spec.label+" subscription decode failed; resubscribing", "err", err)
			phase = phase.Transition(SubscriptionResubscribing)
			if !resubscribeLoop(ctx, c, stream, &lastChangeID, out, spec, &phase) {
				return
			}
			continue
		}
		if ev.Error != nil {
			slog.Debug("registry "+spec.label+" subscription stream error; resubscribing", "err", *ev.Error)
			phase = phase.Transition(SubscriptionResubscribing)
			if !resubscribeLoop(ctx, c, stream, &lastChangeID, out, spec, &phase) {
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
			phase = phase.Transition(SubscriptionResubscribing)
			if !resubscribeLoop(ctx, c, stream, &lastChangeID, out, spec, &phase) {
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
	phase *SubscriptionPhase,
) bool {
	stream.Body.Close()

	backoff := time.Second
	for attempt := range maxResubscribeAttempts {
		select {
		case <-ctx.Done():
			if phase != nil {
				*phase = phase.Transition(SubscriptionClosedContext)
			}
			return false
		case <-time.After(backoff):
		}

		next, err := c.resubscribe(ctx, stream.ID, *lastChangeID)
		if err == nil {
			stream.Body = next.Body
			stream.Decoder = next.Decoder
			if phase != nil {
				*phase = phase.Transition(SubscriptionStreaming)
			}
			slog.Info("registry "+spec.label+" subscription restored", "change_id", *lastChangeID)
			select {
			case <-ctx.Done():
				if phase != nil {
					*phase = phase.Transition(SubscriptionClosedContext)
				}
				stream.Body.Close()
				return false
			case out <- spec.resyncMsg:
			}
			return true
		}

		slog.Debug("registry "+spec.label+" resubscribe failed", "change_id", *lastChangeID, "attempt", attempt+1, "backoff", backoff.String(), "err", err)
		backoff = min(backoff*2, maxResubscribeBackoff)
	}
	if phase != nil {
		*phase = phase.Transition(SubscriptionClosedExhausted)
	}
	slog.Warn("registry "+spec.label+" resubscribe exhausted retries", "change_id", *lastChangeID, "attempts", maxResubscribeAttempts)
	return false
}
