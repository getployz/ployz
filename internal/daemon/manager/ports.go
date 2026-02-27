package manager

import (
	"context"

	"ployz/internal/daemon/convergence"
	"ployz/internal/daemon/overlay"
	"ployz/pkg/sdk/types"
)

// SpecStore persists network specs with enabled/disabled state.
type SpecStore interface {
	SaveSpec(spec types.NetworkSpec, enabled bool) error
	GetSpec() (PersistedSpec, bool, error)
	DeleteSpec() error
	Close() error
}

// PersistedSpec holds a network spec and its enabled state.
type PersistedSpec struct {
	Spec    types.NetworkSpec
	Enabled bool
}

// OverlayService encapsulates one-shot infrastructure lifecycle operations.
type OverlayService interface {
	Start(ctx context.Context, in overlay.Config) (overlay.Config, error)
	Stop(ctx context.Context, in overlay.Config, purge bool) (overlay.Config, error)
	Status(ctx context.Context, in overlay.Config) (overlay.Status, error)
	ListContainers(ctx context.Context, labelFilter map[string]string) ([]overlay.ContainerListEntry, error)
	Close() error
}

// MembershipService encapsulates machine CRUD operations.
type MembershipService interface {
	ListMachines(ctx context.Context, cfg overlay.Config) ([]overlay.Machine, error)
	UpsertMachine(ctx context.Context, cfg overlay.Config, machine overlay.Machine) error
	RemoveMachine(ctx context.Context, cfg overlay.Config, machineID string) error
	Close() error
}

// ConvergenceService encapsulates continuous reconciliation and health reporting.
type ConvergenceService interface {
	Start(ctx context.Context, spec types.NetworkSpec) error
	Stop() error
	StopAll()
	Status() (convergence.SupervisorPhase, string)
	Health() convergence.NetworkHealth
}

// WorkloadService encapsulates deploy operations (currently stubbed).
type WorkloadService interface {
	PlanDeploy(ctx context.Context, namespace string, composeSpec []byte) (types.DeployPlan, error)
	ApplyDeploy(ctx context.Context, namespace string, composeSpec []byte, events chan<- types.DeployProgressEvent) (types.DeployResult, error)
	ListDeployments(ctx context.Context, namespace string) ([]types.DeploymentEntry, error)
	RemoveNamespace(ctx context.Context, namespace string) error
	ReadContainerState(ctx context.Context, namespace string) ([]types.ContainerState, error)
}
