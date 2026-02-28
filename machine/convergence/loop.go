// Package convergence watches the registry for membership changes
// and syncs WireGuard peers to match.
//
// TODO: Peer reachability tracking. Convergence should probe WireGuard peers
// (handshake age) and mark unreachable ones as down. This drives the health
// model: "expected members" is the count of reachable peers, not the store
// total. Without this, a node that restarts after a peer was removed will
// never satisfy a members>=N health gate because the dead peer won't respond
// to gossip. See platform/corrorun/ready.go for why startup doesn't gate on
// cluster health, and docs/corrosion-health.md for the health endpoint.
package convergence

import (
	"context"
	"fmt"
	"log/slog"

	"ployz"
	"ployz/internal/check"
)

// Loop watches the registry and reconciles WireGuard peers.
// It owns its goroutine lifecycle via Start/Stop.
type Loop struct {
	self       ployz.MachineRecord
	planner    PeerPlanner
	subscriber Subscriber
	peers      PeerSetter

	cancel context.CancelFunc
	done   chan struct{}
}

// New creates a convergence loop.
func New(self ployz.MachineRecord, planner PeerPlanner, subscriber Subscriber, peers PeerSetter) *Loop {
	return &Loop{
		self:       self,
		planner:    planner,
		subscriber: subscriber,
		peers:      peers,
	}
}

// Start launches the convergence loop in a background goroutine.
// The loop subscribes to the registry and syncs peers on every change.
func (l *Loop) Start(ctx context.Context) error {
	ctx, l.cancel = context.WithCancel(ctx)
	l.done = make(chan struct{})

	go func() {
		defer close(l.done)
		if err := l.run(ctx); err != nil {
			slog.Error("convergence loop exited", "err", err)
		}
	}()

	return nil
}

// Stop cancels the convergence loop and waits for it to exit.
func (l *Loop) Stop() error {
	if l.cancel != nil {
		l.cancel()
		<-l.done
	}
	return nil
}

func (l *Loop) run(ctx context.Context) error {
	records, changes, err := l.subscriber.SubscribeMachines(ctx)
	if err != nil {
		return fmt.Errorf("subscribe to registry: %w", err)
	}

	if err := l.reconcile(ctx, records); err != nil {
		return fmt.Errorf("initial peer sync: %w", err)
	}

	for {
		select {
		case <-ctx.Done():
			return nil
		case event, ok := <-changes:
			if !ok {
				return fmt.Errorf("registry subscription lost")
			}
			records = applyEvent(records, event)
			if err := l.reconcile(ctx, records); err != nil {
				slog.Error("peer sync failed", "err", err)
			}
		}
	}
}

func (l *Loop) reconcile(ctx context.Context, records []ployz.MachineRecord) error {
	planned := l.planner.PlanPeers(l.self, records)
	return l.peers.SetPeers(ctx, planned)
}

// applyEvent returns a new record set with the event applied.
func applyEvent(records []ployz.MachineRecord, event ployz.MachineEvent) []ployz.MachineRecord {
	switch event.Kind {
	case ployz.MachineAdded:
		return append(records, event.Record)
	case ployz.MachineUpdated:
		for i, record := range records {
			if record.ID == event.Record.ID {
				records[i] = event.Record
				return records
			}
		}
		return append(records, event.Record)
	case ployz.MachineRemoved:
		out := make([]ployz.MachineRecord, 0, len(records))
		for _, record := range records {
			if record.ID != event.Record.ID {
				out = append(out, record)
			}
		}
		return out
	default:
		check.Assertf(false, "unknown machine event kind: %d", event.Kind)
	}
	return records
}
