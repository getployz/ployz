package sqlite

import (
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"ployz/internal/daemon/overlay"
	"ployz/pkg/sdk/defaults"
)

// NetworkStateStore implements overlay.StateStore using SQLite.
type NetworkStateStore struct{}

var _ overlay.StateStore = NetworkStateStore{}

func (NetworkStateStore) Load(dataDir string) (*overlay.State, error) {
	net := networkFromDataDir(dataDir)
	if net == "" {
		return nil, fmt.Errorf("resolve network name from data dir %q", dataDir)
	}

	db, err := openMachineDB(machineDBPath(dataDir))
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, os.ErrNotExist
		}
		return nil, fmt.Errorf("open machine db: %w", err)
	}
	defer db.Close()

	const query = `
SELECT
	network,
	coalesce(cidr, ''),
	subnet,
	management,
	coalesce(advertise, ''),
	wg_interface,
	wg_port,
	wg_private,
	wg_public,
	coalesce(host_wg_private, ''),
	coalesce(host_wg_public, ''),
	docker_network,
	corrosion_name,
	corrosion_img,
	coalesce(corrosion_member_id, 0),
	coalesce(corrosion_api_token, ''),
	coalesce(bootstrap_json, '[]'),
	coalesce(runtime_phase, ''),
	running
FROM network_state
WHERE network = ?`

	var s overlay.State
	var memberID int64
	var bootstrapJSON string
	var runtimePhase string
	var running int
	if err := db.QueryRow(query, net).Scan(
		&s.Network,
		&s.CIDR,
		&s.Subnet,
		&s.Management,
		&s.Advertise,
		&s.WGInterface,
		&s.WGPort,
		&s.WGPrivate,
		&s.WGPublic,
		&s.HostWGPrivate,
		&s.HostWGPublic,
		&s.DockerNetwork,
		&s.CorrosionName,
		&s.CorrosionImage,
		&memberID,
		&s.CorrosionAPIToken,
		&bootstrapJSON,
		&runtimePhase,
		&running,
	); err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return nil, os.ErrNotExist
		}
		return nil, fmt.Errorf("read machine state: %w", err)
	}

	s.Phase = parseRuntimePhase(runtimePhase, running)
	if memberID > 0 {
		s.CorrosionMemberID = uint64(memberID)
	}
	if err := json.Unmarshal([]byte(bootstrapJSON), &s.Bootstrap); err != nil {
		return nil, fmt.Errorf("parse state bootstrap: %w", err)
	}

	managementIP, err := overlay.ManagementIPFromPublicKey(s.WGPublic)
	if err != nil {
		return nil, fmt.Errorf("derive management IP from state key: %w", err)
	}
	s.Management = managementIP.String()

	return &s, nil
}

func (NetworkStateStore) Save(dataDir string, s *overlay.State) error {
	net := networkFromDataDir(dataDir)
	if net == "" {
		return fmt.Errorf("resolve network name from data dir %q", dataDir)
	}
	if s.Network == "" {
		s.Network = net
	}
	if err := defaults.EnsureDataRoot(filepath.Dir(machineDBPath(dataDir))); err != nil {
		return fmt.Errorf("create machine db dir: %w", err)
	}

	db, err := openMachineDB(machineDBPath(dataDir))
	if err != nil {
		return fmt.Errorf("open machine db: %w", err)
	}
	defer db.Close()

	bootstrapJSON, err := json.Marshal(s.Bootstrap)
	if err != nil {
		return fmt.Errorf("marshal state bootstrap: %w", err)
	}

	const upsert = `
INSERT INTO network_state (
	network,
	cidr,
	subnet,
	management,
	advertise,
	wg_interface,
	wg_port,
	wg_private,
	wg_public,
	host_wg_private,
	host_wg_public,
	docker_network,
	corrosion_name,
	corrosion_img,
	corrosion_member_id,
	corrosion_api_token,
	bootstrap_json,
	runtime_phase,
	running,
	updated_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(network) DO UPDATE SET
	cidr = excluded.cidr,
	subnet = excluded.subnet,
	management = excluded.management,
	advertise = excluded.advertise,
	wg_interface = excluded.wg_interface,
	wg_port = excluded.wg_port,
	wg_private = excluded.wg_private,
	wg_public = excluded.wg_public,
	host_wg_private = excluded.host_wg_private,
	host_wg_public = excluded.host_wg_public,
	docker_network = excluded.docker_network,
	corrosion_name = excluded.corrosion_name,
	corrosion_img = excluded.corrosion_img,
	corrosion_member_id = excluded.corrosion_member_id,
	corrosion_api_token = excluded.corrosion_api_token,
	bootstrap_json = excluded.bootstrap_json,
	runtime_phase = excluded.runtime_phase,
	running = excluded.running,
	updated_at = excluded.updated_at`

	if s.Phase == 0 {
		s.Phase = overlay.NetworkStopped
	}

	if _, err := db.Exec(
		upsert,
		s.Network,
		s.CIDR,
		s.Subnet,
		s.Management,
		s.Advertise,
		s.WGInterface,
		s.WGPort,
		s.WGPrivate,
		s.WGPublic,
		s.HostWGPrivate,
		s.HostWGPublic,
		s.DockerNetwork,
		s.CorrosionName,
		s.CorrosionImage,
		s.CorrosionMemberID,
		s.CorrosionAPIToken,
		string(bootstrapJSON),
		s.Phase.String(),
		boolToInt(s.Phase == overlay.NetworkRunning),
		time.Now().UTC().Format(time.RFC3339),
	); err != nil {
		return fmt.Errorf("write machine state: %w", err)
	}
	return nil
}

func (NetworkStateStore) Delete(dataDir string) error {
	net := networkFromDataDir(dataDir)
	if net == "" {
		return fmt.Errorf("resolve network name from data dir %q", dataDir)
	}

	db, err := openMachineDB(machineDBPath(dataDir))
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil
		}
		return fmt.Errorf("open machine db: %w", err)
	}
	defer db.Close()

	if _, err := db.Exec(`DELETE FROM network_state WHERE network = ?`, net); err != nil {
		return fmt.Errorf("delete machine state: %w", err)
	}
	return nil
}

// StatePath returns the display path for the state DB entry.
func (NetworkStateStore) StatePath(dataDir string) string {
	dbPath := machineDBPath(dataDir)
	net := networkFromDataDir(dataDir)
	if net == "" {
		return dbPath
	}
	return dbPath + "#" + net
}

func openMachineDB(path string) (*sql.DB, error) {
	if _, err := os.Stat(path); err != nil {
		if errors.Is(err, os.ErrNotExist) {
			if err := defaults.EnsureDataRoot(filepath.Dir(path)); err != nil {
				return nil, fmt.Errorf("create machine db directory: %w", err)
			}
		} else {
			return nil, err
		}
	}

	db, err := openDB(path)
	if err != nil {
		return nil, err
	}

	const schema = `
CREATE TABLE IF NOT EXISTS network_state (
	network TEXT PRIMARY KEY,
	cidr TEXT,
	subnet TEXT NOT NULL,
	management TEXT NOT NULL,
	advertise TEXT,
	wg_interface TEXT NOT NULL,
	wg_port INTEGER NOT NULL,
	wg_private TEXT NOT NULL,
	wg_public TEXT NOT NULL,
	host_wg_private TEXT NOT NULL DEFAULT '',
	host_wg_public TEXT NOT NULL DEFAULT '',
	docker_network TEXT NOT NULL,
	corrosion_name TEXT NOT NULL,
	corrosion_img TEXT NOT NULL,
	corrosion_member_id INTEGER NOT NULL DEFAULT 0,
	corrosion_api_token TEXT NOT NULL DEFAULT '',
	bootstrap_json TEXT NOT NULL DEFAULT '[]',
	runtime_phase TEXT NOT NULL DEFAULT '',
	running INTEGER NOT NULL DEFAULT 0,
	updated_at TEXT NOT NULL
)`
	if _, err := db.Exec(schema); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("initialize machine db schema: %w", err)
	}
	if _, err := db.Exec(`ALTER TABLE network_state ADD COLUMN runtime_phase TEXT NOT NULL DEFAULT ''`); err != nil {
		_ = err
	}

	return db, nil
}

func machineDBPath(dataDir string) string {
	return filepath.Join(filepath.Dir(filepath.Clean(dataDir)), "machine.db")
}

func networkFromDataDir(dataDir string) string {
	n := filepath.Base(filepath.Clean(dataDir))
	if n == "." || n == "/" {
		return ""
	}
	return n
}

func parseRuntimePhase(raw string, running int) overlay.NetworkRuntimePhase {
	switch raw {
	case "unconfigured":
		return overlay.NetworkUnconfigured
	case "stopped":
		return overlay.NetworkStopped
	case "starting":
		return overlay.NetworkStarting
	case "running":
		return overlay.NetworkRunning
	case "stopping":
		return overlay.NetworkStopping
	case "purged":
		return overlay.NetworkPurged
	case "failed":
		return overlay.NetworkFailed
	default:
		if running != 0 {
			return overlay.NetworkRunning
		}
		return overlay.NetworkStopped
	}
}
