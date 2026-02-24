package corrosion

import (
	"context"
	"encoding/json"
	"fmt"
	"sort"
	"strings"

	"ployz/internal/deploy"
)

const (
	deploymentsTable = "deployments"
	containersTable  = "containers"
)

var (
	_ deploy.ContainerStore  = Store{}
	_ deploy.DeploymentStore = Store{}
)

func (s Store) EnsureContainerTable(ctx context.Context) error {
	query := fmt.Sprintf(`
CREATE TABLE IF NOT EXISTS %s (
    id TEXT NOT NULL PRIMARY KEY,
    namespace TEXT NOT NULL,
    deploy_id TEXT NOT NULL,
    service TEXT NOT NULL,
    machine_id TEXT NOT NULL,
    container_name TEXT NOT NULL,
    spec_json TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'running',
    version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
)`, containersTable)
	return s.exec(ctx, query)
}

func (s Store) InsertContainer(ctx context.Context, row deploy.ContainerRow) error {
	if strings.TrimSpace(row.ID) == "" {
		return fmt.Errorf("insert container: id is required")
	}
	if row.Version <= 0 {
		row.Version = 1
	}
	query := fmt.Sprintf(
		"INSERT INTO %s (id, namespace, deploy_id, service, machine_id, container_name, spec_json, status, version, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
		containersTable,
	)
	return s.exec(ctx, query,
		row.ID,
		row.Namespace,
		row.DeployID,
		row.Service,
		row.MachineID,
		row.ContainerName,
		row.SpecJSON,
		row.Status,
		row.Version,
		row.CreatedAt,
		row.UpdatedAt,
	)
}

func (s Store) UpdateContainer(ctx context.Context, row deploy.ContainerRow) error {
	if strings.TrimSpace(row.ID) == "" {
		return fmt.Errorf("update container: id is required")
	}
	query := fmt.Sprintf("UPDATE %s SET spec_json = ?, status = ?, version = version + 1, updated_at = ? WHERE id = ?", containersTable)
	return s.exec(ctx, query, row.SpecJSON, row.Status, row.UpdatedAt, row.ID)
}

func (s Store) ListContainersByNamespace(ctx context.Context, namespace string) ([]deploy.ContainerRow, error) {
	query := fmt.Sprintf(
		"SELECT id, namespace, deploy_id, service, machine_id, container_name, spec_json, status, version, created_at, updated_at FROM %s WHERE namespace = ? ORDER BY service, machine_id, container_name",
		containersTable,
	)
	rows, err := s.query(ctx, query, namespace)
	if err != nil {
		return nil, err
	}
	out := make([]deploy.ContainerRow, 0, len(rows))
	for _, values := range rows {
		decoded, decodeErr := decodeContainerRow(values)
		if decodeErr != nil {
			return nil, decodeErr
		}
		out = append(out, decoded)
	}
	return out, nil
}

func (s Store) ListContainersByDeploy(ctx context.Context, namespace, deployID string) ([]deploy.ContainerRow, error) {
	query := fmt.Sprintf(
		"SELECT id, namespace, deploy_id, service, machine_id, container_name, spec_json, status, version, created_at, updated_at FROM %s WHERE namespace = ? AND deploy_id = ? ORDER BY service, machine_id, container_name",
		containersTable,
	)
	rows, err := s.query(ctx, query, namespace, deployID)
	if err != nil {
		return nil, err
	}
	out := make([]deploy.ContainerRow, 0, len(rows))
	for _, values := range rows {
		decoded, decodeErr := decodeContainerRow(values)
		if decodeErr != nil {
			return nil, decodeErr
		}
		out = append(out, decoded)
	}
	return out, nil
}

func (s Store) DeleteContainer(ctx context.Context, id string) error {
	query := fmt.Sprintf("DELETE FROM %s WHERE id = ?", containersTable)
	return s.exec(ctx, query, id)
}

func (s Store) DeleteContainersByNamespace(ctx context.Context, namespace string) error {
	query := fmt.Sprintf("DELETE FROM %s WHERE namespace = ?", containersTable)
	return s.exec(ctx, query, namespace)
}

func (s Store) EnsureDeploymentTable(ctx context.Context) error {
	query := fmt.Sprintf(`
CREATE TABLE IF NOT EXISTS %s (
    id TEXT NOT NULL PRIMARY KEY,
    namespace TEXT NOT NULL,
    spec_json TEXT NOT NULL,
    labels_json TEXT NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'in_progress',
    owner TEXT,
    owner_heartbeat TEXT,
    machine_ids_json TEXT NOT NULL DEFAULT '[]',
    version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
)`, deploymentsTable)
	return s.exec(ctx, query)
}

func (s Store) InsertDeployment(ctx context.Context, row deploy.DeploymentRow) error {
	if strings.TrimSpace(row.ID) == "" {
		return fmt.Errorf("insert deployment: id is required")
	}
	if row.Version <= 0 {
		row.Version = 1
	}
	labelsJSON, err := marshalJSONString(normalizedLabels(row.Labels), "deployment labels")
	if err != nil {
		return err
	}
	machineIDsJSON, err := marshalJSONString(normalizedMachineIDs(row.MachineIDs), "deployment machine ids")
	if err != nil {
		return err
	}
	status, err := deploymentStatusString(row.Status)
	if err != nil {
		return fmt.Errorf("insert deployment: %w", err)
	}
	query := fmt.Sprintf(
		"INSERT INTO %s (id, namespace, spec_json, labels_json, status, owner, owner_heartbeat, machine_ids_json, version, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
		deploymentsTable,
	)
	return s.exec(ctx, query,
		row.ID,
		row.Namespace,
		row.SpecJSON,
		labelsJSON,
		status,
		row.Owner,
		row.OwnerHeartbeat,
		machineIDsJSON,
		row.Version,
		row.CreatedAt,
		row.UpdatedAt,
	)
}

func (s Store) UpdateDeployment(ctx context.Context, row deploy.DeploymentRow) error {
	if strings.TrimSpace(row.ID) == "" {
		return fmt.Errorf("update deployment: id is required")
	}
	labelsJSON, err := marshalJSONString(normalizedLabels(row.Labels), "deployment labels")
	if err != nil {
		return err
	}
	machineIDsJSON, err := marshalJSONString(normalizedMachineIDs(row.MachineIDs), "deployment machine ids")
	if err != nil {
		return err
	}
	status, err := deploymentStatusString(row.Status)
	if err != nil {
		return fmt.Errorf("update deployment: %w", err)
	}
	query := fmt.Sprintf(
		"UPDATE %s SET spec_json = ?, labels_json = ?, status = ?, owner = ?, owner_heartbeat = ?, machine_ids_json = ?, version = version + 1, updated_at = ? WHERE id = ?",
		deploymentsTable,
	)
	return s.exec(ctx, query,
		row.SpecJSON,
		labelsJSON,
		status,
		row.Owner,
		row.OwnerHeartbeat,
		machineIDsJSON,
		row.UpdatedAt,
		row.ID,
	)
}

func (s Store) GetDeployment(ctx context.Context, id string) (deploy.DeploymentRow, bool, error) {
	query := fmt.Sprintf(
		"SELECT id, namespace, spec_json, labels_json, status, owner, owner_heartbeat, machine_ids_json, version, created_at, updated_at FROM %s WHERE id = ?",
		deploymentsTable,
	)
	rows, err := s.query(ctx, query, id)
	if err != nil {
		return deploy.DeploymentRow{}, false, err
	}
	if len(rows) == 0 {
		return deploy.DeploymentRow{}, false, nil
	}
	decoded, decodeErr := decodeDeploymentRow(rows[0])
	if decodeErr != nil {
		return deploy.DeploymentRow{}, false, decodeErr
	}
	return decoded, true, nil
}

func (s Store) GetActiveDeployment(ctx context.Context, namespace string) (deploy.DeploymentRow, bool, error) {
	activeStatus, err := deploymentStatusString(deploy.DeployInProgress)
	if err != nil {
		return deploy.DeploymentRow{}, false, fmt.Errorf("get active deployment status: %w", err)
	}
	query := fmt.Sprintf(
		"SELECT id, namespace, spec_json, labels_json, status, owner, owner_heartbeat, machine_ids_json, version, created_at, updated_at FROM %s WHERE namespace = ? AND status = ? ORDER BY created_at DESC LIMIT 1",
		deploymentsTable,
	)
	rows, err := s.query(ctx, query, namespace, activeStatus)
	if err != nil {
		return deploy.DeploymentRow{}, false, err
	}
	if len(rows) == 0 {
		return deploy.DeploymentRow{}, false, nil
	}
	decoded, decodeErr := decodeDeploymentRow(rows[0])
	if decodeErr != nil {
		return deploy.DeploymentRow{}, false, decodeErr
	}
	return decoded, true, nil
}

func (s Store) ListByNamespace(ctx context.Context, namespace string) ([]deploy.DeploymentRow, error) {
	query := fmt.Sprintf(
		"SELECT id, namespace, spec_json, labels_json, status, owner, owner_heartbeat, machine_ids_json, version, created_at, updated_at FROM %s WHERE namespace = ? ORDER BY created_at DESC",
		deploymentsTable,
	)
	rows, err := s.query(ctx, query, namespace)
	if err != nil {
		return nil, err
	}
	out := make([]deploy.DeploymentRow, 0, len(rows))
	for _, values := range rows {
		decoded, decodeErr := decodeDeploymentRow(values)
		if decodeErr != nil {
			return nil, decodeErr
		}
		out = append(out, decoded)
	}
	return out, nil
}

func (s Store) LatestSuccessful(ctx context.Context, namespace string) (deploy.DeploymentRow, bool, error) {
	successStatus, err := deploymentStatusString(deploy.DeploySucceeded)
	if err != nil {
		return deploy.DeploymentRow{}, false, fmt.Errorf("latest successful deployment status: %w", err)
	}
	query := fmt.Sprintf(
		"SELECT id, namespace, spec_json, labels_json, status, owner, owner_heartbeat, machine_ids_json, version, created_at, updated_at FROM %s WHERE namespace = ? AND status = ? ORDER BY created_at DESC LIMIT 1",
		deploymentsTable,
	)
	rows, err := s.query(ctx, query, namespace, successStatus)
	if err != nil {
		return deploy.DeploymentRow{}, false, err
	}
	if len(rows) == 0 {
		return deploy.DeploymentRow{}, false, nil
	}
	decoded, decodeErr := decodeDeploymentRow(rows[0])
	if decodeErr != nil {
		return deploy.DeploymentRow{}, false, decodeErr
	}
	return decoded, true, nil
}

func (s Store) DeleteDeployment(ctx context.Context, id string) error {
	query := fmt.Sprintf("DELETE FROM %s WHERE id = ?", deploymentsTable)
	return s.exec(ctx, query, id)
}

func (s Store) AcquireOwnership(ctx context.Context, deployID, machineID, now string) error {
	query := fmt.Sprintf(
		"UPDATE %s SET owner = ?, owner_heartbeat = ?, updated_at = ? WHERE id = ? AND (owner IS NULL OR owner = ?)",
		deploymentsTable,
	)
	if err := s.exec(ctx, query, machineID, now, now, deployID, machineID); err != nil {
		return fmt.Errorf("acquire ownership for deployment %s: %w", deployID, err)
	}
	row, ok, err := s.GetDeployment(ctx, deployID)
	if err != nil {
		return fmt.Errorf("acquire ownership verify %s: %w", deployID, err)
	}
	if !ok {
		return fmt.Errorf("acquire ownership: deployment %s not found", deployID)
	}
	if row.Owner != machineID {
		return fmt.Errorf("deployment %s owned by %s", deployID, row.Owner)
	}
	return nil
}

func (s Store) CheckOwnership(ctx context.Context, deployID, machineID string) error {
	row, ok, err := s.GetDeployment(ctx, deployID)
	if err != nil {
		return fmt.Errorf("check ownership for deployment %s: %w", deployID, err)
	}
	if !ok {
		return fmt.Errorf("deployment %s not found", deployID)
	}
	if row.Owner != machineID {
		return fmt.Errorf("deployment %s owned by %s", deployID, row.Owner)
	}
	return nil
}

func (s Store) BumpOwnershipHeartbeat(ctx context.Context, deployID, machineID, now string) error {
	query := fmt.Sprintf(
		"UPDATE %s SET owner_heartbeat = ?, updated_at = ? WHERE id = ? AND owner = ?",
		deploymentsTable,
	)
	if err := s.exec(ctx, query, now, now, deployID, machineID); err != nil {
		return fmt.Errorf("bump ownership heartbeat for deployment %s: %w", deployID, err)
	}
	return s.CheckOwnership(ctx, deployID, machineID)
}

func (s Store) ReleaseOwnership(ctx context.Context, deployID string) error {
	query := fmt.Sprintf("UPDATE %s SET owner = NULL, owner_heartbeat = NULL WHERE id = ?", deploymentsTable)
	if err := s.exec(ctx, query, deployID); err != nil {
		return fmt.Errorf("release ownership for deployment %s: %w", deployID, err)
	}
	return nil
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
