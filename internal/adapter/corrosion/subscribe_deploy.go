package corrosion

import (
	"context"
	"fmt"
	"strings"

	"ployz/internal/deploy"
	"ployz/internal/network"
)

type DeploymentChange struct {
	Kind       network.ChangeKind
	Deployment deploy.DeploymentRow
}

type ContainerChange struct {
	Kind      network.ChangeKind
	Container deploy.ContainerRow
}

var deploymentSpec = subscriptionSpec[deploy.DeploymentRow, DeploymentChange]{
	label:     "deployment",
	decodeRow: decodeDeploymentRow,
	makeChange: func(kind network.ChangeKind, row deploy.DeploymentRow) DeploymentChange {
		return DeploymentChange{Kind: kind, Deployment: row}
	},
	resyncMsg: DeploymentChange{Kind: network.ChangeResync},
}

var containerSpec = subscriptionSpec[deploy.ContainerRow, ContainerChange]{
	label:     "container",
	decodeRow: decodeContainerRow,
	makeChange: func(kind network.ChangeKind, row deploy.ContainerRow) ContainerChange {
		return ContainerChange{Kind: kind, Container: row}
	},
	resyncMsg: ContainerChange{Kind: network.ChangeResync},
}

func (s Store) SubscribeDeployments(ctx context.Context, namespace string) ([]deploy.DeploymentRow, <-chan DeploymentChange, error) {
	return s.Deployments().SubscribeDeployments(ctx, namespace)
}

func (r DeploymentRepo) SubscribeDeployments(ctx context.Context, namespace string) ([]deploy.DeploymentRow, <-chan DeploymentChange, error) {
	namespace = strings.TrimSpace(namespace)
	if namespace == "" {
		query := fmt.Sprintf("SELECT id, namespace, spec_json, labels_json, status, owner, owner_heartbeat, machine_ids_json, version, created_at, updated_at FROM %s ORDER BY created_at DESC", deploymentsTable)
		return openAndRun(ctx, r.client, query, nil, deploymentSpec)
	}
	query := fmt.Sprintf("SELECT id, namespace, spec_json, labels_json, status, owner, owner_heartbeat, machine_ids_json, version, created_at, updated_at FROM %s WHERE namespace = ? ORDER BY created_at DESC", deploymentsTable)
	return openAndRun(ctx, r.client, query, []any{namespace}, deploymentSpec)
}

func (s Store) SubscribeContainers(ctx context.Context, namespace string) ([]deploy.ContainerRow, <-chan ContainerChange, error) {
	return s.Containers().SubscribeContainers(ctx, namespace)
}

func (r ContainerRepo) SubscribeContainers(ctx context.Context, namespace string) ([]deploy.ContainerRow, <-chan ContainerChange, error) {
	namespace = strings.TrimSpace(namespace)
	if namespace == "" {
		query := fmt.Sprintf("SELECT id, namespace, deploy_id, service, machine_id, container_name, spec_json, status, version, created_at, updated_at FROM %s ORDER BY namespace, service, machine_id, container_name", containersTable)
		return openAndRun(ctx, r.client, query, nil, containerSpec)
	}
	query := fmt.Sprintf("SELECT id, namespace, deploy_id, service, machine_id, container_name, spec_json, status, version, created_at, updated_at FROM %s WHERE namespace = ? ORDER BY service, machine_id, container_name", containersTable)
	return openAndRun(ctx, r.client, query, []any{namespace}, containerSpec)
}
