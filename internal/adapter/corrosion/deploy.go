package corrosion

import (
	"context"
	"encoding/json"
	"fmt"
	"sort"

	"ployz/internal/deploy"
)

const (
	deploymentsTable = "deployments"
	containersTable  = "containers"
)

func (s Store) EnsureContainerTable(ctx context.Context) error {
	return s.Containers().EnsureContainerTable(ctx)
}

func (s Store) InsertContainer(ctx context.Context, row deploy.ContainerRow) error {
	return s.Containers().InsertContainer(ctx, row)
}

func (s Store) UpdateContainer(ctx context.Context, row deploy.ContainerRow) error {
	return s.Containers().UpdateContainer(ctx, row)
}

func (s Store) ListContainersByNamespace(ctx context.Context, namespace string) ([]deploy.ContainerRow, error) {
	return s.Containers().ListContainersByNamespace(ctx, namespace)
}

func (s Store) ListContainers(ctx context.Context) ([]deploy.ContainerRow, error) {
	return s.Containers().ListContainers(ctx)
}

func (s Store) ListContainersByDeploy(ctx context.Context, namespace, deployID string) ([]deploy.ContainerRow, error) {
	return s.Containers().ListContainersByDeploy(ctx, namespace, deployID)
}

func (s Store) DeleteContainer(ctx context.Context, id string) error {
	return s.Containers().DeleteContainer(ctx, id)
}

func (s Store) DeleteContainersByNamespace(ctx context.Context, namespace string) error {
	return s.Containers().DeleteContainersByNamespace(ctx, namespace)
}

func (s Store) EnsureDeploymentTable(ctx context.Context) error {
	return s.Deployments().EnsureDeploymentTable(ctx)
}

func (s Store) InsertDeployment(ctx context.Context, row deploy.DeploymentRow) error {
	return s.Deployments().InsertDeployment(ctx, row)
}

func (s Store) UpdateDeployment(ctx context.Context, row deploy.DeploymentRow) error {
	return s.Deployments().UpdateDeployment(ctx, row)
}

func (s Store) GetDeployment(ctx context.Context, id string) (deploy.DeploymentRow, bool, error) {
	return s.Deployments().GetDeployment(ctx, id)
}

func (s Store) GetActiveDeployment(ctx context.Context, namespace string) (deploy.DeploymentRow, bool, error) {
	return s.Deployments().GetActiveDeployment(ctx, namespace)
}

func (s Store) ListByNamespace(ctx context.Context, namespace string) ([]deploy.DeploymentRow, error) {
	return s.Deployments().ListByNamespace(ctx, namespace)
}

func (s Store) ListAll(ctx context.Context) ([]deploy.DeploymentRow, error) {
	return s.Deployments().ListAll(ctx)
}

func (s Store) LatestSuccessful(ctx context.Context, namespace string) (deploy.DeploymentRow, bool, error) {
	return s.Deployments().LatestSuccessful(ctx, namespace)
}

func (s Store) DeleteDeployment(ctx context.Context, id string) error {
	return s.Deployments().DeleteDeployment(ctx, id)
}

func (s Store) AcquireOwnership(ctx context.Context, deployID, machineID, now string) error {
	return s.Deployments().AcquireOwnership(ctx, deployID, machineID, now)
}

func (s Store) CheckOwnership(ctx context.Context, deployID, machineID string) error {
	return s.Deployments().CheckOwnership(ctx, deployID, machineID)
}

func (s Store) BumpOwnershipHeartbeat(ctx context.Context, deployID, machineID, now string) error {
	return s.Deployments().BumpOwnershipHeartbeat(ctx, deployID, machineID, now)
}

func (s Store) ReleaseOwnership(ctx context.Context, deployID string) error {
	return s.Deployments().ReleaseOwnership(ctx, deployID)
}

func marshalJSONString(v any, label string) (string, error) {
	data, err := json.Marshal(v)
	if err != nil {
		return "", fmt.Errorf("marshal %s: %w", label, err)
	}
	return string(data), nil
}

func deploymentStatusString(status deploy.DeployPhase) (string, error) {
	if !status.IsValid() {
		return "", fmt.Errorf("invalid deployment status phase: %d", status)
	}
	return status.String(), nil
}

func decodeContainerRow(values []json.RawMessage) (deploy.ContainerRow, error) {
	if len(values) != 11 {
		return deploy.ContainerRow{}, fmt.Errorf("decode container row: expected 11 columns, got %d", len(values))
	}

	var out deploy.ContainerRow
	var err error
	if out.ID, err = decodeString(values[0], "container id"); err != nil {
		return deploy.ContainerRow{}, err
	}
	if out.Namespace, err = decodeString(values[1], "container namespace"); err != nil {
		return deploy.ContainerRow{}, err
	}
	if out.DeployID, err = decodeString(values[2], "container deploy_id"); err != nil {
		return deploy.ContainerRow{}, err
	}
	if out.Service, err = decodeString(values[3], "container service"); err != nil {
		return deploy.ContainerRow{}, err
	}
	if out.MachineID, err = decodeString(values[4], "container machine_id"); err != nil {
		return deploy.ContainerRow{}, err
	}
	if out.ContainerName, err = decodeString(values[5], "container name"); err != nil {
		return deploy.ContainerRow{}, err
	}
	if out.SpecJSON, err = decodeString(values[6], "container spec_json"); err != nil {
		return deploy.ContainerRow{}, err
	}
	if out.Status, err = decodeString(values[7], "container status"); err != nil {
		return deploy.ContainerRow{}, err
	}
	if out.Version, err = decodeInt64(values[8], "container version"); err != nil {
		return deploy.ContainerRow{}, err
	}
	if out.CreatedAt, err = decodeString(values[9], "container created_at"); err != nil {
		return deploy.ContainerRow{}, err
	}
	if out.UpdatedAt, err = decodeString(values[10], "container updated_at"); err != nil {
		return deploy.ContainerRow{}, err
	}
	if out.Version <= 0 {
		out.Version = 1
	}
	return out, nil
}

func decodeDeploymentRow(values []json.RawMessage) (deploy.DeploymentRow, error) {
	if len(values) != 11 {
		return deploy.DeploymentRow{}, fmt.Errorf("decode deployment row: expected 11 columns, got %d", len(values))
	}

	var out deploy.DeploymentRow
	var err error
	if out.ID, err = decodeString(values[0], "deployment id"); err != nil {
		return deploy.DeploymentRow{}, err
	}
	if out.Namespace, err = decodeString(values[1], "deployment namespace"); err != nil {
		return deploy.DeploymentRow{}, err
	}
	if out.SpecJSON, err = decodeString(values[2], "deployment spec_json"); err != nil {
		return deploy.DeploymentRow{}, err
	}
	labelsJSON, err := decodeString(values[3], "deployment labels_json")
	if err != nil {
		return deploy.DeploymentRow{}, err
	}
	statusRaw, err := decodeString(values[4], "deployment status")
	if err != nil {
		return deploy.DeploymentRow{}, err
	}
	phase, ok := deploy.ParseDeployPhase(statusRaw)
	if !ok {
		return deploy.DeploymentRow{}, fmt.Errorf("decode deployment status: invalid value %q", statusRaw)
	}
	out.Status = phase
	if out.Owner, err = decodeString(values[5], "deployment owner"); err != nil {
		return deploy.DeploymentRow{}, err
	}
	if out.OwnerHeartbeat, err = decodeString(values[6], "deployment owner_heartbeat"); err != nil {
		return deploy.DeploymentRow{}, err
	}
	machineIDsJSON, err := decodeString(values[7], "deployment machine_ids_json")
	if err != nil {
		return deploy.DeploymentRow{}, err
	}
	if out.Version, err = decodeInt64(values[8], "deployment version"); err != nil {
		return deploy.DeploymentRow{}, err
	}
	if out.CreatedAt, err = decodeString(values[9], "deployment created_at"); err != nil {
		return deploy.DeploymentRow{}, err
	}
	if out.UpdatedAt, err = decodeString(values[10], "deployment updated_at"); err != nil {
		return deploy.DeploymentRow{}, err
	}

	if labelsJSON == "" {
		labelsJSON = "{}"
	}
	if machineIDsJSON == "" {
		machineIDsJSON = "[]"
	}
	if err := json.Unmarshal([]byte(labelsJSON), &out.Labels); err != nil {
		return deploy.DeploymentRow{}, fmt.Errorf("decode deployment labels_json: %w", err)
	}
	if err := json.Unmarshal([]byte(machineIDsJSON), &out.MachineIDs); err != nil {
		return deploy.DeploymentRow{}, fmt.Errorf("decode deployment machine_ids_json: %w", err)
	}
	if out.Labels == nil {
		out.Labels = map[string]string{}
	}
	if out.MachineIDs == nil {
		out.MachineIDs = []string{}
	}
	if out.Version <= 0 {
		out.Version = 1
	}
	return out, nil
}

func normalizedLabels(labels map[string]string) map[string]string {
	if len(labels) == 0 {
		return map[string]string{}
	}
	out := make(map[string]string, len(labels))
	for key, value := range labels {
		out[key] = value
	}
	return out
}

func normalizedMachineIDs(ids []string) []string {
	if len(ids) == 0 {
		return []string{}
	}
	out := append([]string(nil), ids...)
	sort.Strings(out)
	return out
}
