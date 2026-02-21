package state

import (
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	pb "ployz/internal/daemon/pb"

	_ "modernc.org/sqlite"
)

type PersistedSpec struct {
	Spec    *pb.NetworkSpec
	Enabled bool
}

type RuntimeStatus struct {
	Network   string
	Running   bool
	LastError string
	UpdatedAt string
}

type Store struct {
	db *sql.DB
}

func Open(path string) (*Store, error) {
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return nil, fmt.Errorf("create state directory: %w", err)
	}

	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, fmt.Errorf("open state db: %w", err)
	}
	if _, err := db.Exec(`PRAGMA journal_mode = WAL`); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("set state db journal mode: %w", err)
	}
	if _, err := db.Exec(`PRAGMA busy_timeout = 5000`); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("set state db busy timeout: %w", err)
	}
	if _, err := db.Exec(`
CREATE TABLE IF NOT EXISTS network_specs (
	network TEXT PRIMARY KEY,
	spec_json TEXT NOT NULL,
	enabled INTEGER NOT NULL DEFAULT 1,
	updated_at TEXT NOT NULL
)`); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("initialize network specs schema: %w", err)
	}
	if _, err := db.Exec(`
CREATE TABLE IF NOT EXISTS runtime_status (
	network TEXT PRIMARY KEY,
	running INTEGER NOT NULL DEFAULT 0,
	last_error TEXT NOT NULL DEFAULT '',
	updated_at TEXT NOT NULL
)`); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("initialize runtime status schema: %w", err)
	}

	return &Store{db: db}, nil
}

func (s *Store) Close() error {
	if s == nil || s.db == nil {
		return nil
	}
	return s.db.Close()
}

func (s *Store) ListSpecs() ([]PersistedSpec, error) {
	rows, err := s.db.Query(`SELECT network, spec_json, enabled FROM network_specs ORDER BY network`)
	if err != nil {
		return nil, fmt.Errorf("list network specs: %w", err)
	}
	defer rows.Close()

	out := make([]PersistedSpec, 0)
	for rows.Next() {
		var network string
		var specJSON string
		var enabled int
		if err := rows.Scan(&network, &specJSON, &enabled); err != nil {
			return nil, fmt.Errorf("scan network spec row: %w", err)
		}
		spec := &pb.NetworkSpec{}
		if err := json.Unmarshal([]byte(specJSON), spec); err != nil {
			return nil, fmt.Errorf("unmarshal network spec %q: %w", network, err)
		}
		if strings.TrimSpace(spec.Network) == "" {
			spec.Network = network
		}
		out = append(out, PersistedSpec{Spec: spec, Enabled: enabled != 0})
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("iterate network spec rows: %w", err)
	}
	return out, nil
}

func (s *Store) GetSpec(network string) (PersistedSpec, bool, error) {
	var specJSON string
	var enabled int
	err := s.db.QueryRow(`SELECT spec_json, enabled FROM network_specs WHERE network = ?`, network).Scan(&specJSON, &enabled)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return PersistedSpec{}, false, nil
		}
		return PersistedSpec{}, false, fmt.Errorf("query network spec %q: %w", network, err)
	}

	spec := &pb.NetworkSpec{}
	if err := json.Unmarshal([]byte(specJSON), spec); err != nil {
		return PersistedSpec{}, false, fmt.Errorf("unmarshal network spec %q: %w", network, err)
	}
	if strings.TrimSpace(spec.Network) == "" {
		spec.Network = network
	}
	return PersistedSpec{Spec: spec, Enabled: enabled != 0}, true, nil
}

func (s *Store) SaveSpec(spec *pb.NetworkSpec, enabled bool) error {
	payload, err := json.Marshal(spec)
	if err != nil {
		return fmt.Errorf("marshal network spec: %w", err)
	}
	enabledInt := 0
	if enabled {
		enabledInt = 1
	}

	network := strings.TrimSpace(spec.Network)
	_, err = s.db.Exec(
		`INSERT INTO network_specs (network, spec_json, enabled, updated_at)
		 VALUES (?, ?, ?, ?)
		 ON CONFLICT(network) DO UPDATE SET
		 spec_json = excluded.spec_json,
		 enabled = excluded.enabled,
		 updated_at = excluded.updated_at`,
		network,
		string(payload),
		enabledInt,
		time.Now().UTC().Format(time.RFC3339),
	)
	if err != nil {
		return fmt.Errorf("save network spec: %w", err)
	}
	if !enabled {
		if setErr := s.SetRuntimeStatus(network, false, ""); setErr != nil {
			return setErr
		}
	}
	return nil
}

func (s *Store) DeleteSpec(network string) error {
	if _, err := s.db.Exec(`DELETE FROM network_specs WHERE network = ?`, network); err != nil {
		return fmt.Errorf("delete network spec: %w", err)
	}
	if _, err := s.db.Exec(`DELETE FROM runtime_status WHERE network = ?`, network); err != nil {
		return fmt.Errorf("delete runtime status: %w", err)
	}
	return nil
}

func (s *Store) SetRuntimeStatus(network string, running bool, lastError string) error {
	network = strings.TrimSpace(network)
	if network == "" {
		return fmt.Errorf("network is required")
	}
	runningInt := 0
	if running {
		runningInt = 1
	}
	_, err := s.db.Exec(
		`INSERT INTO runtime_status (network, running, last_error, updated_at)
		 VALUES (?, ?, ?, ?)
		 ON CONFLICT(network) DO UPDATE SET
		 running = excluded.running,
		 last_error = excluded.last_error,
		 updated_at = excluded.updated_at`,
		network,
		runningInt,
		strings.TrimSpace(lastError),
		time.Now().UTC().Format(time.RFC3339),
	)
	if err != nil {
		return fmt.Errorf("set runtime status: %w", err)
	}
	return nil
}

func (s *Store) GetRuntimeStatus(network string) (RuntimeStatus, bool, error) {
	var running int
	out := RuntimeStatus{Network: network}
	err := s.db.QueryRow(
		`SELECT running, last_error, updated_at FROM runtime_status WHERE network = ?`,
		network,
	).Scan(&running, &out.LastError, &out.UpdatedAt)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return RuntimeStatus{}, false, nil
		}
		return RuntimeStatus{}, false, fmt.Errorf("query runtime status %q: %w", network, err)
	}
	out.Running = running != 0
	return out, true, nil
}
