package corrosion

import (
	"context"
	"fmt"
	"net/netip"
	"strings"

	"ployz/internal/deploy"
)

var _ deploy.ContainerStore = ContainerRepo{}

// ContainerRepo provides containers-table operations over Corrosion.
type ContainerRepo struct {
	client corrosionClient
}

// NewContainerRepo creates a container repository from Corrosion API coordinates.
func NewContainerRepo(apiAddr netip.AddrPort, apiToken string) ContainerRepo {
	return NewStore(apiAddr, apiToken).Containers()
}

// Containers returns a container-scoped repository backed by this Store.
func (s Store) Containers() ContainerRepo {
	return ContainerRepo{client: s.clientOrDefault()}
}

func (r ContainerRepo) EnsureContainerTable(ctx context.Context) error {
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
	return r.client.exec(ctx, query)
}

func (r ContainerRepo) InsertContainer(ctx context.Context, row deploy.ContainerRow) error {
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
	return r.client.exec(ctx, query,
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

func (r ContainerRepo) UpdateContainer(ctx context.Context, row deploy.ContainerRow) error {
	if strings.TrimSpace(row.ID) == "" {
		return fmt.Errorf("update container: id is required")
	}
	query := fmt.Sprintf("UPDATE %s SET spec_json = ?, status = ?, version = version + 1, updated_at = ? WHERE id = ?", containersTable)
	return r.client.exec(ctx, query, row.SpecJSON, row.Status, row.UpdatedAt, row.ID)
}

func (r ContainerRepo) ListContainersByNamespace(ctx context.Context, namespace string) ([]deploy.ContainerRow, error) {
	query := fmt.Sprintf(
		"SELECT id, namespace, deploy_id, service, machine_id, container_name, spec_json, status, version, created_at, updated_at FROM %s WHERE namespace = ? ORDER BY service, machine_id, container_name",
		containersTable,
	)
	rows, err := r.client.query(ctx, query, namespace)
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

func (r ContainerRepo) ListContainers(ctx context.Context) ([]deploy.ContainerRow, error) {
	query := fmt.Sprintf(
		"SELECT id, namespace, deploy_id, service, machine_id, container_name, spec_json, status, version, created_at, updated_at FROM %s ORDER BY namespace, deploy_id, service, machine_id, container_name",
		containersTable,
	)
	rows, err := r.client.query(ctx, query)
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

func (r ContainerRepo) ListContainersByDeploy(ctx context.Context, namespace, deployID string) ([]deploy.ContainerRow, error) {
	query := fmt.Sprintf(
		"SELECT id, namespace, deploy_id, service, machine_id, container_name, spec_json, status, version, created_at, updated_at FROM %s WHERE namespace = ? AND deploy_id = ? ORDER BY service, machine_id, container_name",
		containersTable,
	)
	rows, err := r.client.query(ctx, query, namespace, deployID)
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

func (r ContainerRepo) DeleteContainer(ctx context.Context, id string) error {
	query := fmt.Sprintf("DELETE FROM %s WHERE id = ?", containersTable)
	return r.client.exec(ctx, query, id)
}

func (r ContainerRepo) DeleteContainersByNamespace(ctx context.Context, namespace string) error {
	query := fmt.Sprintf("DELETE FROM %s WHERE namespace = ?", containersTable)
	return r.client.exec(ctx, query, namespace)
}
