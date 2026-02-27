package manager

import (
	"context"

	"ployz/pkg/sdk/types"
)

func (m *Manager) PlanDeploy(ctx context.Context, namespace string, composeSpec []byte) (types.DeployPlan, error) {
	return m.workload.PlanDeploy(ctx, namespace, composeSpec)
}

func (m *Manager) ApplyDeploy(ctx context.Context, namespace string, composeSpec []byte, events chan<- types.DeployProgressEvent) (types.DeployResult, error) {
	return m.workload.ApplyDeploy(ctx, namespace, composeSpec, events)
}

func (m *Manager) ListDeployments(ctx context.Context, namespace string) ([]types.DeploymentEntry, error) {
	return m.workload.ListDeployments(ctx, namespace)
}

func (m *Manager) RemoveNamespace(ctx context.Context, namespace string) error {
	return m.workload.RemoveNamespace(ctx, namespace)
}

func (m *Manager) ReadContainerState(ctx context.Context, namespace string) ([]types.ContainerState, error) {
	return m.workload.ReadContainerState(ctx, namespace)
}
