package corrosion

import (
	"context"
	"fmt"
	"net/netip"
	"strings"

	"ployz/internal/deploy"
)

var _ deploy.DeploymentStore = DeploymentRepo{}

// DeploymentRepo provides deployments-table operations over Corrosion.
type DeploymentRepo struct {
	client corrosionClient
}

// NewDeploymentRepo creates a deployment repository from Corrosion API coordinates.
func NewDeploymentRepo(apiAddr netip.AddrPort, apiToken string) DeploymentRepo {
	return NewStore(apiAddr, apiToken).Deployments()
}

// Deployments returns a deployment-scoped repository backed by this Store.
func (s Store) Deployments() DeploymentRepo {
	return DeploymentRepo{client: s.clientOrDefault()}
}

func (r DeploymentRepo) EnsureDeploymentTable(ctx context.Context) error {
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
	return r.client.exec(ctx, query)
}

func (r DeploymentRepo) InsertDeployment(ctx context.Context, row deploy.DeploymentRow) error {
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
	return r.client.exec(ctx, query,
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

func (r DeploymentRepo) UpdateDeployment(ctx context.Context, row deploy.DeploymentRow) error {
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
	return r.client.exec(ctx, query,
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

func (r DeploymentRepo) GetDeployment(ctx context.Context, id string) (deploy.DeploymentRow, bool, error) {
	query := fmt.Sprintf(
		"SELECT id, namespace, spec_json, labels_json, status, owner, owner_heartbeat, machine_ids_json, version, created_at, updated_at FROM %s WHERE id = ?",
		deploymentsTable,
	)
	rows, err := r.client.query(ctx, query, id)
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

func (r DeploymentRepo) GetActiveDeployment(ctx context.Context, namespace string) (deploy.DeploymentRow, bool, error) {
	activeStatus, err := deploymentStatusString(deploy.DeployInProgress)
	if err != nil {
		return deploy.DeploymentRow{}, false, fmt.Errorf("get active deployment status: %w", err)
	}
	query := fmt.Sprintf(
		"SELECT id, namespace, spec_json, labels_json, status, owner, owner_heartbeat, machine_ids_json, version, created_at, updated_at FROM %s WHERE namespace = ? AND status = ? ORDER BY created_at DESC LIMIT 1",
		deploymentsTable,
	)
	rows, err := r.client.query(ctx, query, namespace, activeStatus)
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

func (r DeploymentRepo) ListByNamespace(ctx context.Context, namespace string) ([]deploy.DeploymentRow, error) {
	query := fmt.Sprintf(
		"SELECT id, namespace, spec_json, labels_json, status, owner, owner_heartbeat, machine_ids_json, version, created_at, updated_at FROM %s WHERE namespace = ? ORDER BY created_at DESC",
		deploymentsTable,
	)
	rows, err := r.client.query(ctx, query, namespace)
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

func (r DeploymentRepo) ListAll(ctx context.Context) ([]deploy.DeploymentRow, error) {
	query := fmt.Sprintf(
		"SELECT id, namespace, spec_json, labels_json, status, owner, owner_heartbeat, machine_ids_json, version, created_at, updated_at FROM %s ORDER BY namespace, created_at DESC",
		deploymentsTable,
	)
	rows, err := r.client.query(ctx, query)
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

func (r DeploymentRepo) LatestSuccessful(ctx context.Context, namespace string) (deploy.DeploymentRow, bool, error) {
	successStatus, err := deploymentStatusString(deploy.DeploySucceeded)
	if err != nil {
		return deploy.DeploymentRow{}, false, fmt.Errorf("latest successful deployment status: %w", err)
	}
	query := fmt.Sprintf(
		"SELECT id, namespace, spec_json, labels_json, status, owner, owner_heartbeat, machine_ids_json, version, created_at, updated_at FROM %s WHERE namespace = ? AND status = ? ORDER BY created_at DESC LIMIT 1",
		deploymentsTable,
	)
	rows, err := r.client.query(ctx, query, namespace, successStatus)
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

func (r DeploymentRepo) DeleteDeployment(ctx context.Context, id string) error {
	query := fmt.Sprintf("DELETE FROM %s WHERE id = ?", deploymentsTable)
	return r.client.exec(ctx, query, id)
}

func (r DeploymentRepo) AcquireOwnership(ctx context.Context, deployID, machineID, now string) error {
	query := fmt.Sprintf(
		"UPDATE %s SET owner = ?, owner_heartbeat = ?, updated_at = ? WHERE id = ? AND (owner IS NULL OR owner = ?)",
		deploymentsTable,
	)
	if err := r.client.exec(ctx, query, machineID, now, now, deployID, machineID); err != nil {
		return fmt.Errorf("acquire ownership for deployment %s: %w", deployID, err)
	}
	row, ok, err := r.GetDeployment(ctx, deployID)
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

func (r DeploymentRepo) CheckOwnership(ctx context.Context, deployID, machineID string) error {
	row, ok, err := r.GetDeployment(ctx, deployID)
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

func (r DeploymentRepo) BumpOwnershipHeartbeat(ctx context.Context, deployID, machineID, now string) error {
	query := fmt.Sprintf(
		"UPDATE %s SET owner_heartbeat = ?, updated_at = ? WHERE id = ? AND owner = ?",
		deploymentsTable,
	)
	if err := r.client.exec(ctx, query, now, now, deployID, machineID); err != nil {
		return fmt.Errorf("bump ownership heartbeat for deployment %s: %w", deployID, err)
	}
	return r.CheckOwnership(ctx, deployID, machineID)
}

func (r DeploymentRepo) ReleaseOwnership(ctx context.Context, deployID string) error {
	query := fmt.Sprintf("UPDATE %s SET owner = NULL, owner_heartbeat = NULL WHERE id = ?", deploymentsTable)
	if err := r.client.exec(ctx, query, deployID); err != nil {
		return fmt.Errorf("release ownership for deployment %s: %w", deployID, err)
	}
	return nil
}
