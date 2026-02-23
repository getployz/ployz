package deploy

import "context"

// ContainerStore abstracts Corrosion CRUD for the containers table.
type ContainerStore interface {
	EnsureContainerTable(ctx context.Context) error
	InsertContainer(ctx context.Context, row ContainerRow) error
	UpdateContainer(ctx context.Context, row ContainerRow) error
	ListContainersByNamespace(ctx context.Context, namespace string) ([]ContainerRow, error)
	ListContainersByDeploy(ctx context.Context, namespace, deployID string) ([]ContainerRow, error)
	DeleteContainer(ctx context.Context, id string) error
	DeleteContainersByNamespace(ctx context.Context, namespace string) error
}

// DeploymentStore abstracts Corrosion CRUD for deployments and ownership.
type DeploymentStore interface {
	EnsureDeploymentTable(ctx context.Context) error
	InsertDeployment(ctx context.Context, row DeploymentRow) error
	UpdateDeployment(ctx context.Context, row DeploymentRow) error
	GetDeployment(ctx context.Context, id string) (DeploymentRow, bool, error)
	GetActiveDeployment(ctx context.Context, namespace string) (DeploymentRow, bool, error)
	ListByNamespace(ctx context.Context, namespace string) ([]DeploymentRow, error)
	LatestSuccessful(ctx context.Context, namespace string) (DeploymentRow, bool, error)
	DeleteDeployment(ctx context.Context, id string) error

	AcquireOwnership(ctx context.Context, deployID, machineID, now string) error
	CheckOwnership(ctx context.Context, deployID, machineID string) error
	BumpOwnershipHeartbeat(ctx context.Context, deployID, machineID, now string) error
	ReleaseOwnership(ctx context.Context, deployID string) error
}

// HealthChecker polls container health checks.
type HealthChecker interface {
	WaitHealthy(ctx context.Context, containerName string, cfg HealthCheck) error
}

// StateReader reads actual container state from Docker on a specific machine.
type StateReader interface {
	ReadMachineState(ctx context.Context, machineID, namespace string) ([]ContainerState, error)
}

// ContainerState is the actual state of a container read from Docker.
type ContainerState struct {
	ContainerName string
	Image         string
	Running       bool
	Healthy       bool
}

// Stores groups persistence interfaces for convenience.
type Stores struct {
	Containers  ContainerStore
	Deployments DeploymentStore
}
