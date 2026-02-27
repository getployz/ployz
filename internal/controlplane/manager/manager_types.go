package manager

import (
	"context"
	"net/netip"

	"ployz/internal/deploy"
	"ployz/internal/engine"
	"ployz/internal/network"
	"ployz/internal/observed"
	"ployz/pkg/sdk/client"
)

// Compile-time check: Manager implements client.API.
var _ client.API = (*Manager)(nil)

type Manager struct {
	ctx                         context.Context
	dataRoot                    string
	store                       SpecStore
	stateStore                  network.StateStore
	ctrl                        *network.Controller
	engine                      *engine.Engine
	newStores                   DeployStoresFactory
	runtimeStore                observed.ContainerStore
	runtimeCursorStore          observed.SyncCursorStore
	controlPlaneWorkloadSummary ControlPlaneWorkloadSummaryFunc
	attachedMachinesSummary     AttachedMachinesSummaryFunc
}

type managerCfg struct {
	specStore                   SpecStore
	stateStore                  network.StateStore
	ctrl                        *network.Controller
	eng                         *engine.Engine
	newStores                   DeployStoresFactory
	runtimeStore                observed.ContainerStore
	runtimeCursorStore          observed.SyncCursorStore
	controlPlaneWorkloadSummary ControlPlaneWorkloadSummaryFunc
	attachedMachinesSummary     AttachedMachinesSummaryFunc
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
func WithManagerStateStore(s network.StateStore) ManagerOption {
	return func(c *managerCfg) { c.stateStore = s }
}

// WithManagerController injects a pre-built Controller.
func WithManagerController(ctrl *network.Controller) ManagerOption {
	return func(c *managerCfg) { c.ctrl = ctrl }
}

// WithManagerEngine injects a pre-built Engine.
func WithManagerEngine(e *engine.Engine) ManagerOption {
	return func(c *managerCfg) { c.eng = e }
}

// WithDeployStoresFactory injects deploy store construction for Corrosion-backed operations.
func WithDeployStoresFactory(f DeployStoresFactory) ManagerOption {
	return func(c *managerCfg) { c.newStores = f }
}

// WithRuntimeContainerStore injects local observed runtime container metadata storage.
func WithRuntimeContainerStore(s observed.ContainerStore) ManagerOption {
	return func(c *managerCfg) { c.runtimeStore = s }
}

// WithRuntimeSyncCursorStore injects local runtime observation cursor storage.
func WithRuntimeSyncCursorStore(s observed.SyncCursorStore) ManagerOption {
	return func(c *managerCfg) { c.runtimeCursorStore = s }
}

// WithControlPlaneWorkloadSummary injects control-plane workload summary logic.
// This is primarily useful for tests that should avoid Corrosion HTTP calls.
func WithControlPlaneWorkloadSummary(f ControlPlaneWorkloadSummaryFunc) ManagerOption {
	return func(c *managerCfg) { c.controlPlaneWorkloadSummary = f }
}

// WithAttachedMachinesSummary injects attached machine summary logic for
// network destroy preflights.
func WithAttachedMachinesSummary(f AttachedMachinesSummaryFunc) ManagerOption {
	return func(c *managerCfg) { c.attachedMachinesSummary = f }
}

// DeployStoresFactory creates deploy stores from Corrosion connection details.
type DeployStoresFactory func(addr netip.AddrPort, token string) deploy.Stores

// ControlPlaneWorkloadSummaryFunc summarizes managed workload presence in
// control-plane container rows.
type ControlPlaneWorkloadSummaryFunc func(ctx context.Context, cfg network.Config) (count int, contexts []string, err error)

// AttachedMachinesSummaryFunc summarizes remote machines still attached to the
// active network.
type AttachedMachinesSummaryFunc func(ctx context.Context, cfg network.Config) (count int, machineIDs []string, err error)
