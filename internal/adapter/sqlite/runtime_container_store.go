package sqlite

import (
	"context"
	"database/sql"
	"fmt"
	"strings"
	"time"

	"ployz/internal/observed"
)

// RuntimeContainerStore persists machine-local observed runtime container data.
type RuntimeContainerStore struct{}

var _ observed.ContainerStore = RuntimeContainerStore{}

func (RuntimeContainerStore) ReplaceNamespaceSnapshot(
	ctx context.Context,
	dataDir, machineID, namespace string,
	rows []observed.ContainerRecord,
	observedAt time.Time,
) error {
	if strings.TrimSpace(dataDir) == "" {
		return fmt.Errorf("data dir is required")
	}
	machineID = strings.TrimSpace(machineID)
	if machineID == "" {
		return fmt.Errorf("machine id is required")
	}
	namespace = strings.TrimSpace(namespace)
	if namespace == "" {
		return fmt.Errorf("namespace is required")
	}

	db, err := openMachineDB(machineDBPath(dataDir))
	if err != nil {
		return fmt.Errorf("open machine db: %w", err)
	}
	defer db.Close()

	if err := ensureRuntimeContainerSchema(db); err != nil {
		return err
	}

	if observedAt.IsZero() {
		observedAt = time.Now().UTC()
	}
	observedAtRaw := observedAt.UTC().Format(time.RFC3339Nano)

	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("begin runtime container snapshot transaction: %w", err)
	}
	defer func() {
		_ = tx.Rollback()
	}()

	if _, err := tx.ExecContext(ctx, `DELETE FROM runtime_containers WHERE machine_id = ? AND namespace = ?`, machineID, namespace); err != nil {
		return fmt.Errorf("delete previous runtime container snapshot: %w", err)
	}
	if _, err := tx.ExecContext(ctx, `DELETE FROM runtime_container_ports WHERE machine_id = ? AND namespace = ?`, machineID, namespace); err != nil {
		return fmt.Errorf("delete previous runtime container port snapshot: %w", err)
	}

	stmt, err := tx.PrepareContext(ctx, `
INSERT INTO runtime_containers (
	machine_id,
	namespace,
	deploy_id,
	container_name,
	image,
	running,
	healthy,
	observed_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?)`)
	if err != nil {
		return fmt.Errorf("prepare runtime container snapshot insert: %w", err)
	}
	defer stmt.Close()

	portStmt, err := tx.PrepareContext(ctx, `
INSERT INTO runtime_container_ports (
	machine_id,
	namespace,
	container_name,
	host_ip,
	host_port,
	container_port,
	protocol,
	observed_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?)`)
	if err != nil {
		return fmt.Errorf("prepare runtime container port snapshot insert: %w", err)
	}
	defer portStmt.Close()

	for _, row := range rows {
		containerName := strings.TrimSpace(row.ContainerName)
		if containerName == "" {
			return fmt.Errorf("runtime container name is required")
		}
		rowNamespace := strings.TrimSpace(row.Namespace)
		if rowNamespace == "" {
			rowNamespace = namespace
		}
		if _, err := stmt.ExecContext(
			ctx,
			machineID,
			rowNamespace,
			strings.TrimSpace(row.DeployID),
			containerName,
			strings.TrimSpace(row.Image),
			boolToInt(row.Running),
			boolToInt(row.Healthy),
			observedAtRaw,
		); err != nil {
			return fmt.Errorf("insert runtime container snapshot row %q: %w", containerName, err)
		}

		for _, port := range row.Ports {
			protocol := normalizePortProtocol(port.Protocol)
			if _, err := portStmt.ExecContext(
				ctx,
				machineID,
				rowNamespace,
				containerName,
				strings.TrimSpace(port.HostIP),
				int(port.HostPort),
				int(port.ContainerPort),
				protocol,
				observedAtRaw,
			); err != nil {
				return fmt.Errorf("insert runtime container port snapshot row %q: %w", containerName, err)
			}
		}
	}

	if err := tx.Commit(); err != nil {
		return fmt.Errorf("commit runtime container snapshot transaction: %w", err)
	}

	return nil
}

func (RuntimeContainerStore) ListNamespace(ctx context.Context, dataDir, namespace string) ([]observed.ContainerRecord, error) {
	if strings.TrimSpace(dataDir) == "" {
		return nil, fmt.Errorf("data dir is required")
	}
	namespace = strings.TrimSpace(namespace)
	if namespace == "" {
		return nil, fmt.Errorf("namespace is required")
	}

	db, err := openMachineDB(machineDBPath(dataDir))
	if err != nil {
		return nil, fmt.Errorf("open machine db: %w", err)
	}
	defer db.Close()

	if err := ensureRuntimeContainerSchema(db); err != nil {
		return nil, err
	}

	rows, err := db.QueryContext(
		ctx,
		`SELECT machine_id, namespace, deploy_id, container_name, image, running, healthy, observed_at
FROM runtime_containers
WHERE namespace = ?
ORDER BY machine_id, container_name`,
		namespace,
	)
	if err != nil {
		return nil, fmt.Errorf("query runtime containers for namespace %q: %w", namespace, err)
	}
	defer rows.Close()

	out := make([]observed.ContainerRecord, 0)
	for rows.Next() {
		var row observed.ContainerRecord
		var running, healthy int
		if err := rows.Scan(
			&row.MachineID,
			&row.Namespace,
			&row.DeployID,
			&row.ContainerName,
			&row.Image,
			&running,
			&healthy,
			&row.ObservedAt,
		); err != nil {
			return nil, fmt.Errorf("scan runtime container row: %w", err)
		}
		row.Running = running != 0
		row.Healthy = healthy != 0
		out = append(out, row)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("iterate runtime containers: %w", err)
	}
	if len(out) == 0 {
		return out, nil
	}

	rowIndex := make(map[runtimeContainerKey]int, len(out))
	for i, row := range out {
		rowIndex[runtimeContainerKey{MachineID: row.MachineID, Namespace: row.Namespace, ContainerName: row.ContainerName}] = i
	}

	portRows, err := db.QueryContext(
		ctx,
		`SELECT machine_id, namespace, container_name, host_ip, host_port, container_port, protocol
FROM runtime_container_ports
WHERE namespace = ?
ORDER BY machine_id, container_name, host_port, container_port, protocol`,
		namespace,
	)
	if err != nil {
		return nil, fmt.Errorf("query runtime container ports for namespace %q: %w", namespace, err)
	}
	defer portRows.Close()

	for portRows.Next() {
		var machineID string
		var rowNamespace string
		var containerName string
		var hostIP string
		var hostPortRaw int
		var containerPortRaw int
		var protocol string
		if err := portRows.Scan(
			&machineID,
			&rowNamespace,
			&containerName,
			&hostIP,
			&hostPortRaw,
			&containerPortRaw,
			&protocol,
		); err != nil {
			return nil, fmt.Errorf("scan runtime container port row: %w", err)
		}
		hostPort, err := parseStoredPort(hostPortRaw, "host_port")
		if err != nil {
			return nil, err
		}
		containerPort, err := parseStoredPort(containerPortRaw, "container_port")
		if err != nil {
			return nil, err
		}

		idx, ok := rowIndex[runtimeContainerKey{MachineID: machineID, Namespace: rowNamespace, ContainerName: containerName}]
		if !ok {
			continue
		}
		out[idx].Ports = append(out[idx].Ports, observed.ContainerPort{
			HostIP:        strings.TrimSpace(hostIP),
			HostPort:      hostPort,
			ContainerPort: containerPort,
			Protocol:      normalizePortProtocol(protocol),
		})
	}
	if err := portRows.Err(); err != nil {
		return nil, fmt.Errorf("iterate runtime container ports: %w", err)
	}

	return out, nil
}

func ensureRuntimeContainerSchema(db *sql.DB) error {
	const schema = `
CREATE TABLE IF NOT EXISTS runtime_containers (
	machine_id TEXT NOT NULL,
	namespace TEXT NOT NULL,
	deploy_id TEXT NOT NULL DEFAULT '',
	container_name TEXT NOT NULL,
	image TEXT NOT NULL DEFAULT '',
	running INTEGER NOT NULL DEFAULT 0,
	healthy INTEGER NOT NULL DEFAULT 0,
	observed_at TEXT NOT NULL,
	PRIMARY KEY(machine_id, namespace, container_name)
);
CREATE TABLE IF NOT EXISTS runtime_container_ports (
	machine_id TEXT NOT NULL,
	namespace TEXT NOT NULL,
	container_name TEXT NOT NULL,
	host_ip TEXT NOT NULL DEFAULT '',
	host_port INTEGER NOT NULL,
	container_port INTEGER NOT NULL,
	protocol TEXT NOT NULL DEFAULT 'tcp',
	observed_at TEXT NOT NULL,
	PRIMARY KEY(machine_id, namespace, container_name, host_ip, host_port, container_port, protocol)
);
CREATE INDEX IF NOT EXISTS idx_runtime_containers_namespace ON runtime_containers(namespace);
CREATE INDEX IF NOT EXISTS idx_runtime_containers_namespace_deploy ON runtime_containers(namespace, deploy_id);
CREATE INDEX IF NOT EXISTS idx_runtime_container_ports_namespace ON runtime_container_ports(namespace);
CREATE INDEX IF NOT EXISTS idx_runtime_container_ports_container ON runtime_container_ports(namespace, container_name);`
	if _, err := db.Exec(schema); err != nil {
		return fmt.Errorf("initialize runtime container schema: %w", err)
	}
	return nil
}

type runtimeContainerKey struct {
	MachineID     string
	Namespace     string
	ContainerName string
}

func normalizePortProtocol(protocol string) string {
	protocol = strings.ToLower(strings.TrimSpace(protocol))
	if protocol == "" {
		return "tcp"
	}
	return protocol
}

func parseStoredPort(raw int, label string) (uint16, error) {
	if raw < 0 || raw > 65535 {
		return 0, fmt.Errorf("decode runtime %s: value %d out of range", label, raw)
	}
	return uint16(raw), nil
}
