package leaf

import "ployz/internal/adapter/fake"

// Transitional re-export layer for leaf fakes.
// New code should prefer this package over importing fake root directly.

type (
	CallRecorder      = fake.CallRecorder
	Clock             = fake.Clock
	StateStore        = fake.StateStore
	SpecStore         = fake.SpecStore
	ContainerRuntime  = fake.ContainerRuntime
	CorrosionRuntime  = fake.CorrosionRuntime
	PlatformOps       = fake.PlatformOps
	StatusProber      = fake.StatusProber
	NetworkController = fake.NetworkController
	PeerReconciler    = fake.PeerReconciler
	ContainerStore    = fake.ContainerStore
	DeploymentStore   = fake.DeploymentStore
	HealthChecker     = fake.HealthChecker
)

var (
	NewClock             = fake.NewClock
	NewStateStore        = fake.NewStateStore
	NewSpecStore         = fake.NewSpecStore
	NewContainerRuntime  = fake.NewContainerRuntime
	NewCorrosionRuntime  = fake.NewCorrosionRuntime
	NewNetworkController = fake.NewNetworkController
	NewPeerReconciler    = fake.NewPeerReconciler
	NewContainerStore    = fake.NewContainerStore
	NewDeploymentStore   = fake.NewDeploymentStore
	NewHealthChecker     = fake.NewHealthChecker
)
