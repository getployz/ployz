package sqlite

import (
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"path/filepath"
	"strings"
	"time"

	"ployz/pkg/sdk/defaults"
	"ployz/pkg/sdk/types"

	_ "modernc.org/sqlite"
)

type PersistedSpec struct {
	Spec    types.NetworkSpec
	Enabled bool
}

type Store struct {
	db *sql.DB
}

func Open(path string) (*Store, error) {
	if err := defaults.EnsureDataRoot(filepath.Dir(path)); err != nil {
		return nil, fmt.Errorf("create state directory: %w", err)
	}

	db, err := openDB(path)
	if err != nil {
		return nil, fmt.Errorf("open state db: %w", err)
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

	var out []PersistedSpec
	for rows.Next() {
		var network, specJSON string
		var enabled int
		if err := rows.Scan(&network, &specJSON, &enabled); err != nil {
			return nil, fmt.Errorf("scan network spec row: %w", err)
		}
		spec, err := unmarshalSpec(network, specJSON)
		if err != nil {
			return nil, err
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

	spec, err := unmarshalSpec(network, specJSON)
	if err != nil {
		return PersistedSpec{}, false, err
	}
	return PersistedSpec{Spec: spec, Enabled: enabled != 0}, true, nil
}

func (s *Store) SaveSpec(spec types.NetworkSpec, enabled bool) error {
	payload, err := json.Marshal(spec)
	if err != nil {
		return fmt.Errorf("marshal network spec: %w", err)
	}

	_, err = s.db.Exec(
		`INSERT INTO network_specs (network, spec_json, enabled, updated_at)
		 VALUES (?, ?, ?, ?)
		 ON CONFLICT(network) DO UPDATE SET
		 spec_json = excluded.spec_json,
		 enabled = excluded.enabled,
		 updated_at = excluded.updated_at`,
		strings.TrimSpace(spec.Network),
		string(payload),
		boolToInt(enabled),
		time.Now().UTC().Format(time.RFC3339),
	)
	if err != nil {
		return fmt.Errorf("save network spec: %w", err)
	}
	return nil
}

func (s *Store) DeleteSpec(network string) error {
	if _, err := s.db.Exec(`DELETE FROM network_specs WHERE network = ?`, network); err != nil {
		return fmt.Errorf("delete network spec: %w", err)
	}
	return nil
}

// openDB opens a SQLite database with standard pragmas (WAL mode, busy timeout).
func openDB(path string) (*sql.DB, error) {
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, err
	}
	if _, err := db.Exec(`PRAGMA journal_mode = WAL`); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("set journal mode: %w", err)
	}
	if _, err := db.Exec(`PRAGMA busy_timeout = 5000`); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("set busy timeout: %w", err)
	}
	return db, nil
}

// unmarshalSpec decodes a JSON spec and backfills the network name if missing.
func unmarshalSpec(network, specJSON string) (types.NetworkSpec, error) {
	var spec types.NetworkSpec
	if err := json.Unmarshal([]byte(specJSON), &spec); err != nil {
		return types.NetworkSpec{}, fmt.Errorf("unmarshal network spec %q: %w", network, err)
	}
	if strings.TrimSpace(spec.Network) == "" {
		spec.Network = network
	}
	return spec, nil
}

func boolToInt(b bool) int {
	if b {
		return 1
	}
	return 0
}
