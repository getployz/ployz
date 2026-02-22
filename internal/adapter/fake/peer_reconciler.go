package fake

import (
	"context"
	"sync"

	"ployz/internal/network"
	"ployz/internal/reconcile"
)

var _ reconcile.PeerReconciler = (*PeerReconciler)(nil)

// PeerReconciler tracks reconciled peers in memory.
type PeerReconciler struct {
	CallRecorder
	mu       sync.Mutex
	LastRows []network.MachineRow
	Closed   bool

	ReconcilePeersErr func(ctx context.Context, cfg network.Config, rows []network.MachineRow) error
}

// NewPeerReconciler creates a PeerReconciler.
func NewPeerReconciler() *PeerReconciler {
	return &PeerReconciler{}
}

func (r *PeerReconciler) ReconcilePeers(ctx context.Context, cfg network.Config, rows []network.MachineRow) (int, error) {
	r.record("ReconcilePeers", cfg, rows)
	if r.ReconcilePeersErr != nil {
		if err := r.ReconcilePeersErr(ctx, cfg, rows); err != nil {
			return 0, err
		}
	}
	r.mu.Lock()
	defer r.mu.Unlock()

	r.LastRows = make([]network.MachineRow, len(rows))
	copy(r.LastRows, rows)
	return len(rows), nil
}

func (r *PeerReconciler) Close() error {
	r.record("Close")
	r.mu.Lock()
	defer r.mu.Unlock()

	r.Closed = true
	return nil
}
