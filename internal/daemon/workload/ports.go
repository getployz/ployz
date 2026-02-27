package workload

import "context"

type ContainerStore interface {
	EnsureContainerTable(ctx context.Context) error
}

type DeploymentStore interface {
	EnsureDeploymentTable(ctx context.Context) error
}

type HealthChecker interface {
	WaitHealthy(ctx context.Context, containerName string) error
}
