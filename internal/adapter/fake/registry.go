package fake

import (
	"context"
	"net/netip"

	"ployz/internal/check"
	"ployz/internal/mesh"
	"ployz/internal/reconcile"
)

// Compile-time interface assertions.
var (
	_ mesh.Registry   = (*Registry)(nil)
	_ reconcile.Registry = (*Registry)(nil)
)

// Registry is a per-node view into a Cluster. It implements both
// mesh.Registry and reconcile.Registry via structural typing.
type Registry struct {
	CallRecorder
	cluster *Cluster
	nodeID  string

	EnsureMachineTableErr       func(ctx context.Context) error
	EnsureHeartbeatTableErr     func(ctx context.Context) error
	EnsureNetworkConfigTableErr func(ctx context.Context) error
	UpsertMachineErr            func(ctx context.Context, row mesh.MachineRow, ver int64) error
	ListMachineRowsErr          func(ctx context.Context) error
	SubscribeMachinesErr        func(ctx context.Context) error
	SubscribeHeartbeatsErr      func(ctx context.Context) error
	BumpHeartbeatErr            func(ctx context.Context, nodeID string, updatedAt string) error
	DeleteMachineErr            func(ctx context.Context, machineID string) error
	DeleteByEndpointExceptIDErr func(ctx context.Context, endpoint string, id string) error
	EnsureNetworkCIDRErr        func(ctx context.Context) error
}

// NewRegistry creates a Registry for the given node in the cluster.
func NewRegistry(cluster *Cluster, nodeID string) *Registry {
	check.Assert(cluster != nil, "NewRegistry: cluster must not be nil")
	check.Assert(nodeID != "", "NewRegistry: nodeID must not be empty")
	return &Registry{cluster: cluster, nodeID: nodeID}
}

// --- mesh.Registry ---

func (r *Registry) EnsureMachineTable(ctx context.Context) error {
	r.record("EnsureMachineTable")
	if r.cluster.IsKilled(r.nodeID) {
		return ErrNodeDead
	}
	if r.EnsureMachineTableErr != nil {
		return r.EnsureMachineTableErr(ctx)
	}
	return nil
}

func (r *Registry) EnsureNetworkConfigTable(ctx context.Context) error {
	r.record("EnsureNetworkConfigTable")
	if r.cluster.IsKilled(r.nodeID) {
		return ErrNodeDead
	}
	if r.EnsureNetworkConfigTableErr != nil {
		return r.EnsureNetworkConfigTableErr(ctx)
	}
	return nil
}

func (r *Registry) EnsureNetworkCIDR(ctx context.Context, requested netip.Prefix, fallbackCIDR string, defaultCIDR netip.Prefix) (netip.Prefix, error) {
	r.record("EnsureNetworkCIDR", requested, fallbackCIDR, defaultCIDR)
	if r.cluster.IsKilled(r.nodeID) {
		return netip.Prefix{}, ErrNodeDead
	}
	if r.EnsureNetworkCIDRErr != nil {
		if err := r.EnsureNetworkCIDRErr(ctx); err != nil {
			return netip.Prefix{}, err
		}
	}
	return r.cluster.ensureNetworkCIDR(r.nodeID, requested, fallbackCIDR, defaultCIDR)
}

func (r *Registry) UpsertMachine(ctx context.Context, row mesh.MachineRow, expectedVersion int64) error {
	r.record("UpsertMachine", row, expectedVersion)
	if r.cluster.IsKilled(r.nodeID) {
		return ErrNodeDead
	}
	if r.UpsertMachineErr != nil {
		if err := r.UpsertMachineErr(ctx, row, expectedVersion); err != nil {
			return err
		}
	}
	return r.cluster.upsertMachine(r.nodeID, row, expectedVersion)
}

func (r *Registry) DeleteByEndpointExceptID(ctx context.Context, endpoint string, id string) error {
	r.record("DeleteByEndpointExceptID", endpoint, id)
	if r.cluster.IsKilled(r.nodeID) {
		return ErrNodeDead
	}
	if r.DeleteByEndpointExceptIDErr != nil {
		if err := r.DeleteByEndpointExceptIDErr(ctx, endpoint, id); err != nil {
			return err
		}
	}
	r.cluster.deleteByEndpointExceptID(r.nodeID, endpoint, id)
	return nil
}

func (r *Registry) DeleteMachine(ctx context.Context, machineID string) error {
	r.record("DeleteMachine", machineID)
	if r.cluster.IsKilled(r.nodeID) {
		return ErrNodeDead
	}
	if r.DeleteMachineErr != nil {
		if err := r.DeleteMachineErr(ctx, machineID); err != nil {
			return err
		}
	}
	r.cluster.deleteMachine(r.nodeID, machineID)
	return nil
}

func (r *Registry) ListMachineRows(ctx context.Context) ([]mesh.MachineRow, error) {
	r.record("ListMachineRows")
	if r.cluster.IsKilled(r.nodeID) {
		return nil, ErrNodeDead
	}
	if r.ListMachineRowsErr != nil {
		if err := r.ListMachineRowsErr(ctx); err != nil {
			return nil, err
		}
	}
	return r.cluster.listMachines(r.nodeID), nil
}

// --- reconcile.Registry ---

func (r *Registry) EnsureHeartbeatTable(ctx context.Context) error {
	r.record("EnsureHeartbeatTable")
	if r.cluster.IsKilled(r.nodeID) {
		return ErrNodeDead
	}
	if r.EnsureHeartbeatTableErr != nil {
		return r.EnsureHeartbeatTableErr(ctx)
	}
	return nil
}

func (r *Registry) SubscribeMachines(ctx context.Context) ([]mesh.MachineRow, <-chan mesh.MachineChange, error) {
	r.record("SubscribeMachines")
	if r.cluster.IsKilled(r.nodeID) {
		return nil, nil, ErrNodeDead
	}
	if r.SubscribeMachinesErr != nil {
		if err := r.SubscribeMachinesErr(ctx); err != nil {
			return nil, nil, err
		}
	}
	return r.cluster.subscribeMachines(ctx, r.nodeID)
}

func (r *Registry) SubscribeHeartbeats(ctx context.Context) ([]mesh.HeartbeatRow, <-chan mesh.HeartbeatChange, error) {
	r.record("SubscribeHeartbeats")
	if r.cluster.IsKilled(r.nodeID) {
		return nil, nil, ErrNodeDead
	}
	if r.SubscribeHeartbeatsErr != nil {
		if err := r.SubscribeHeartbeatsErr(ctx); err != nil {
			return nil, nil, err
		}
	}
	return r.cluster.subscribeHeartbeats(ctx, r.nodeID)
}

func (r *Registry) BumpHeartbeat(ctx context.Context, nodeID string, updatedAt string) error {
	r.record("BumpHeartbeat", nodeID, updatedAt)
	if r.cluster.IsKilled(r.nodeID) {
		return ErrNodeDead
	}
	if r.BumpHeartbeatErr != nil {
		if err := r.BumpHeartbeatErr(ctx, nodeID, updatedAt); err != nil {
			return err
		}
	}
	r.cluster.bumpHeartbeat(r.nodeID, nodeID, updatedAt)
	return nil
}
