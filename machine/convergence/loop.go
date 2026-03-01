package convergence

import (
	"context"
	"fmt"
	"log/slog"
	"net/netip"
	"sync"
	"time"

	"ployz"
	"ployz/internal/check"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

const probeInterval = 1 * time.Second

// Loop watches the registry and reconciles WireGuard peers.
// It owns its goroutine lifecycle via Start/Stop.
type Loop struct {
	self       ployz.MachineRecord
	planner    PeerPlanner
	subscriber Subscriber
	peers      PeerSetter
	prober     PeerProber

	peerStates map[wgtypes.Key]*peerState

	mu      sync.Mutex // protects summary
	summary ployz.HealthSummary

	cancel context.CancelFunc
	done   chan struct{}
}

// New creates a convergence loop. If prober is nil, health probing is disabled.
func New(self ployz.MachineRecord, planner PeerPlanner, subscriber Subscriber, peers PeerSetter, prober PeerProber) *Loop {
	return &Loop{
		self:       self,
		planner:    planner,
		subscriber: subscriber,
		peers:      peers,
		prober:     prober,
		peerStates: make(map[wgtypes.Key]*peerState),
	}
}

// Health returns the latest peer health summary. Safe for concurrent use.
func (l *Loop) Health() ployz.HealthSummary {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.summary
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

	if err := l.reconcile(ctx, records, nil); err != nil {
		return fmt.Errorf("initial peer sync: %w", err)
	}

	if l.prober == nil {
		for {
			select {
			case <-ctx.Done():
				return nil
			case event, ok := <-changes:
				if !ok {
					return fmt.Errorf("registry subscription lost")
				}
				records = applyEvent(records, event)
				if err := l.reconcile(ctx, records, nil); err != nil {
					slog.Error("peer sync failed", "err", err)
				}
			}
		}
	}

	// Run the first probe immediately so the health summary is initialized
	// without waiting for the ticker. This matters for the bootstrap gate.
	l.probe(ctx, records)

	ticker := time.NewTicker(probeInterval)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return nil
		case event, ok := <-changes:
			if !ok {
				return fmt.Errorf("registry subscription lost")
			}
			records = applyEvent(records, event)
			if err := l.reconcile(ctx, records, nil); err != nil {
				slog.Error("peer sync failed", "err", err)
			}
		case <-ticker.C:
			l.probe(ctx, records)
		}
	}
}

// probe queries WireGuard handshake times, updates peer state, rotates
// endpoints on failure, and publishes a health summary.
func (l *Loop) probe(ctx context.Context, records []ployz.MachineRecord) {
	now := time.Now()

	handshakes, err := l.prober.PeerHandshakes(ctx)
	if err != nil {
		slog.Error("peer handshake probe failed", "err", err)
		return
	}

	// Build set of current peer keys for pruning.
	planned := l.planner.PlanPeers(l.self, records)
	currentKeys := make(map[wgtypes.Key]struct{}, len(planned))

	var rotated []wgtypes.Key

	for i := range planned {
		rec := &planned[i]
		currentKeys[rec.PublicKey] = struct{}{}

		st, ok := l.peerStates[rec.PublicKey]
		if !ok {
			st = &peerState{
				endpointSetAt: now,
			}
			l.peerStates[rec.PublicKey] = st
		}
		st.endpointCount = len(rec.Endpoints)

		if hs, found := handshakes[rec.PublicKey]; found && !hs.IsZero() {
			st.lastHandshake = hs
		}

		if shouldRotate(st, now) {
			oldIdx := st.endpointIndex
			nextEndpoint(st)
			slog.Debug("rotating endpoint",
				"peer", rec.PublicKey,
				"from", oldIdx,
				"to", st.endpointIndex,
			)
			rotated = append(rotated, rec.PublicKey)
		}

		classifyPeer(st, now)
	}

	// Prune state for peers no longer in records.
	for key := range l.peerStates {
		if _, ok := currentKeys[key]; !ok {
			delete(l.peerStates, key)
		}
	}

	// Build health summary.
	summary := ployz.HealthSummary{Initialized: true, Total: len(planned)}
	for _, st := range l.peerStates {
		switch st.health {
		case ployz.PeerNew:
			summary.New++
		case ployz.PeerAlive:
			summary.Alive++
		case ployz.PeerSuspect:
			summary.Suspect++
		}
	}

	l.mu.Lock()
	l.summary = summary
	l.mu.Unlock()

	// If any endpoints rotated, re-reconcile with rotation applied.
	if len(rotated) > 0 {
		rotatedSet := make(map[wgtypes.Key]struct{}, len(rotated))
		for _, k := range rotated {
			rotatedSet[k] = struct{}{}
		}
		if err := l.reconcile(ctx, records, rotatedSet); err != nil {
			slog.Error("peer sync after rotation failed", "err", err)
			// Roll back endpoint indices and endpointSetAt stays unchanged
			// so timers stay accurate.
			for _, k := range rotated {
				if st, ok := l.peerStates[k]; ok {
					// Roll back: go to previous index
					if st.endpointCount > 0 {
						st.endpointIndex = (st.endpointIndex - 1 + st.endpointCount) % st.endpointCount
					}
					if st.endpointsAttempted > 0 {
						st.endpointsAttempted--
					}
				}
			}
		} else {
			// SetPeers succeeded — record when the new endpoints were configured.
			for _, k := range rotated {
				if st, ok := l.peerStates[k]; ok {
					st.endpointSetAt = now
				}
			}
		}
	}
}

// reconcile plans peers and applies them via SetPeers. If rotations is
// non-nil, endpoint ordering for those peers is adjusted so the active
// endpoint (per peerState) is at index [0]. Never mutates the original
// store records.
func (l *Loop) reconcile(ctx context.Context, records []ployz.MachineRecord, rotations map[wgtypes.Key]struct{}) error {
	planned := l.planner.PlanPeers(l.self, records)

	if rotations != nil {
		for i := range planned {
			if _, ok := rotations[planned[i].PublicKey]; !ok {
				continue
			}
			st, ok := l.peerStates[planned[i].PublicKey]
			if !ok || st.endpointIndex == 0 || len(planned[i].Endpoints) <= 1 {
				continue
			}
			// Copy endpoints to avoid mutating the original record.
			eps := make([]netip.AddrPort, len(planned[i].Endpoints))
			copy(eps, planned[i].Endpoints)
			// Move active endpoint to front.
			idx := st.endpointIndex % len(eps)
			eps[0], eps[idx] = eps[idx], eps[0]
			planned[i].Endpoints = eps
		}
	}

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
