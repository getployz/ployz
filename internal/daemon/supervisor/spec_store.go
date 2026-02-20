package supervisor

import (
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"

	pb "ployz/internal/daemon/pb"

	_ "modernc.org/sqlite"
)

type persistedSpec struct {
	Spec    *pb.NetworkSpec
	Enabled bool
}

type specStore struct {
	db *sql.DB
}

func newSpecStore(path string) (*specStore, error) {
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return nil, fmt.Errorf("create daemon state directory: %w", err)
	}

	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, fmt.Errorf("open daemon state db: %w", err)
	}
	if _, err := db.Exec(`PRAGMA journal_mode = WAL`); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("set daemon db journal mode: %w", err)
	}
	if _, err := db.Exec(`PRAGMA busy_timeout = 5000`); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("set daemon db busy timeout: %w", err)
	}
	if _, err := db.Exec(`
CREATE TABLE IF NOT EXISTS network_specs (
	network TEXT PRIMARY KEY,
	spec_json TEXT NOT NULL,
	enabled INTEGER NOT NULL DEFAULT 1,
	updated_at TEXT NOT NULL
)`); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("initialize daemon state schema: %w", err)
	}

	return &specStore{db: db}, nil
}

func (s *specStore) close() error {
	if s == nil || s.db == nil {
		return nil
	}
	return s.db.Close()
}

func (s *specStore) list() ([]persistedSpec, error) {
	rows, err := s.db.Query(`SELECT network, spec_json, enabled FROM network_specs ORDER BY network`)
	if err != nil {
		return nil, fmt.Errorf("list daemon specs: %w", err)
	}
	defer rows.Close()

	out := make([]persistedSpec, 0)
	for rows.Next() {
		var network string
		var specJSON string
		var enabled int
		if err := rows.Scan(&network, &specJSON, &enabled); err != nil {
			return nil, fmt.Errorf("scan daemon spec row: %w", err)
		}
		spec := &pb.NetworkSpec{}
		if err := json.Unmarshal([]byte(specJSON), spec); err != nil {
			return nil, fmt.Errorf("unmarshal daemon spec %q: %w", network, err)
		}
		if spec.Network == "" {
			spec.Network = network
		}
		out = append(out, persistedSpec{Spec: spec, Enabled: enabled != 0})
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("iterate daemon spec rows: %w", err)
	}
	return out, nil
}

func (s *specStore) get(network string) (persistedSpec, bool, error) {
	var specJSON string
	var enabled int
	err := s.db.QueryRow(`SELECT spec_json, enabled FROM network_specs WHERE network = ?`, network).Scan(&specJSON, &enabled)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return persistedSpec{}, false, nil
		}
		return persistedSpec{}, false, fmt.Errorf("query daemon spec %q: %w", network, err)
	}

	spec := &pb.NetworkSpec{}
	if err := json.Unmarshal([]byte(specJSON), spec); err != nil {
		return persistedSpec{}, false, fmt.Errorf("unmarshal daemon spec %q: %w", network, err)
	}
	if spec.Network == "" {
		spec.Network = network
	}
	return persistedSpec{Spec: spec, Enabled: enabled != 0}, true, nil
}

func (s *specStore) save(spec *pb.NetworkSpec, enabled bool) error {
	payload, err := json.Marshal(spec)
	if err != nil {
		return fmt.Errorf("marshal daemon spec: %w", err)
	}
	enabledInt := 0
	if enabled {
		enabledInt = 1
	}

	_, err = s.db.Exec(
		`INSERT INTO network_specs (network, spec_json, enabled, updated_at)
		 VALUES (?, ?, ?, ?)
		 ON CONFLICT(network) DO UPDATE SET
		 spec_json = excluded.spec_json,
		 enabled = excluded.enabled,
		 updated_at = excluded.updated_at`,
		spec.Network,
		string(payload),
		enabledInt,
		time.Now().UTC().Format(time.RFC3339),
	)
	if err != nil {
		return fmt.Errorf("save daemon spec: %w", err)
	}
	return nil
}

func (s *specStore) delete(network string) error {
	if _, err := s.db.Exec(`DELETE FROM network_specs WHERE network = ?`, network); err != nil {
		return fmt.Errorf("delete daemon spec: %w", err)
	}
	return nil
}
