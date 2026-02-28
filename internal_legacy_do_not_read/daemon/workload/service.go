package workload

import (
	"context"
	"fmt"

	"ployz/pkg/sdk/types"
)

const NotYetImplementedMessage = "deploy is being rebuilt - not yet available"

type Service struct{}

func New() *Service {
	return &Service{}
}

func (s *Service) PlanDeploy(ctx context.Context, namespace string, composeSpec []byte) (types.DeployPlan, error) {
	return types.DeployPlan{}, fmt.Errorf("%s", NotYetImplementedMessage)
}

func (s *Service) ApplyDeploy(ctx context.Context, namespace string, composeSpec []byte, events chan<- types.DeployProgressEvent) (types.DeployResult, error) {
	return types.DeployResult{}, fmt.Errorf("%s", NotYetImplementedMessage)
}

func (s *Service) ListDeployments(ctx context.Context, namespace string) ([]types.DeploymentEntry, error) {
	return nil, fmt.Errorf("%s", NotYetImplementedMessage)
}

func (s *Service) RemoveNamespace(ctx context.Context, namespace string) error {
	return fmt.Errorf("%s", NotYetImplementedMessage)
}

func (s *Service) ReadContainerState(ctx context.Context, namespace string) ([]types.ContainerState, error) {
	return nil, fmt.Errorf("%s", NotYetImplementedMessage)
}
