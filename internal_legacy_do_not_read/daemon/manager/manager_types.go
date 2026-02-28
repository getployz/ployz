package manager

import (
	"context"

	"ployz/internal/daemon/overlay"
	"ployz/pkg/sdk/client"
)

// Compile-time check: Manager implements client.API.
var _ client.API = (*Manager)(nil)

type Manager struct {
	ctx                     context.Context
	dataRoot                string
	store                   SpecStore
	stateStore              overlay.StateStore
	overlay                 OverlayService
	membership              MembershipService
	convergence             ConvergenceService
	workload                WorkloadService
	attachedMachinesSummary AttachedMachinesSummaryFunc
}

type managerCfg struct {
	specStore               SpecStore
	stateStore              overlay.StateStore
	overlay                 OverlayService
	membership              MembershipService
	convergence             ConvergenceService
	workload                WorkloadService
	attachedMachinesSummary AttachedMachinesSummaryFunc
}

// ManagerOption configures a Manager.
type ManagerOption func(*managerCfg)

// WithSpecStore injects a SpecStore for New.
// NewProduction wires a sqlite-backed store automatically.
func WithSpecStore(s SpecStore) ManagerOption {
	return func(c *managerCfg) { c.specStore = s }
}

// WithManagerStateStore injects a network.StateStore for New.
// NewProduction wires sqlite.NetworkStateStore automatically.
func WithManagerStateStore(s overlay.StateStore) ManagerOption {
	return func(c *managerCfg) { c.stateStore = s }
}

// WithOverlayService injects a pre-built overlay service.
func WithOverlayService(svc OverlayService) ManagerOption {
	return func(c *managerCfg) { c.overlay = svc }
}

// WithMembershipService injects a pre-built membership service.
func WithMembershipService(svc MembershipService) ManagerOption {
	return func(c *managerCfg) { c.membership = svc }
}

// WithConvergenceService injects a pre-built convergence service.
func WithConvergenceService(svc ConvergenceService) ManagerOption {
	return func(c *managerCfg) { c.convergence = svc }
}

// WithWorkloadService injects a pre-built workload service.
func WithWorkloadService(svc WorkloadService) ManagerOption {
	return func(c *managerCfg) { c.workload = svc }
}

// WithAttachedMachinesSummary injects attached machine summary logic for
// network destroy preflights.
func WithAttachedMachinesSummary(f AttachedMachinesSummaryFunc) ManagerOption {
	return func(c *managerCfg) { c.attachedMachinesSummary = f }
}

// AttachedMachinesSummaryFunc summarizes remote machines still attached to the
// active network.
type AttachedMachinesSummaryFunc func(ctx context.Context, cfg overlay.Config) (count int, machineIDs []string, err error)
