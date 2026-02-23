package fake

import (
	"context"
	"sync"

	"ployz/internal/adapter/fake/fault"
	"ployz/internal/check"
	"ployz/internal/mesh"
	"ployz/internal/reconcile"
)

var _ reconcile.PeerReconciler = (*PeerReconciler)(nil)

const FaultPeerReconcilerReconcilePeers = "peer_reconciler.reconcile_peers"

// PeerReconciler tracks reconciled peers in memory.
type PeerReconciler struct {
	CallRecorder
	mu       sync.Mutex
	LastRows []mesh.MachineRow
	Closed   bool
	faults   *fault.Injector

	ReconcilePeersErr func(ctx context.Context, cfg mesh.Config, rows []mesh.MachineRow) error
}

// NewPeerReconciler creates a PeerReconciler.
func NewPeerReconciler() *PeerReconciler {
	return &PeerReconciler{faults: fault.NewInjector()}
}

func (r *PeerReconciler) FailOnce(point string, err error) {
	r.faults.FailOnce(point, err)
}

func (r *PeerReconciler) FailAlways(point string, err error) {
	r.faults.FailAlways(point, err)
}

func (r *PeerReconciler) SetFaultHook(point string, hook fault.Hook) {
	r.faults.SetHook(point, hook)
}

func (r *PeerReconciler) ClearFault(point string) {
	r.faults.Clear(point)
}

func (r *PeerReconciler) ResetFaults() {
	r.faults.Reset()
}

func (r *PeerReconciler) evalFault(point string, args ...any) error {
	check.Assert(r != nil, "PeerReconciler.evalFault: receiver must not be nil")
	check.Assert(r.faults != nil, "PeerReconciler.evalFault: faults injector must not be nil")
	if r == nil || r.faults == nil {
		return nil
	}
	return r.faults.Eval(point, args...)
}

func (r *PeerReconciler) ReconcilePeers(ctx context.Context, cfg mesh.Config, rows []mesh.MachineRow) (int, error) {
	r.record("ReconcilePeers", cfg, rows)
	if err := r.evalFault(FaultPeerReconcilerReconcilePeers, ctx, cfg, rows); err != nil {
		return 0, err
	}
	if r.ReconcilePeersErr != nil {
		if err := r.ReconcilePeersErr(ctx, cfg, rows); err != nil {
			return 0, err
		}
	}
	r.mu.Lock()
	defer r.mu.Unlock()

	r.LastRows = make([]mesh.MachineRow, len(rows))
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
