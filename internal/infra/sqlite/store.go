package sqlite

import (
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"path/filepath"
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

const (
	activeSpecKey = "active"
)

func Open(path string) (*Store, error) {
	if err := defaults.EnsureDataRoot(filepath.Dir(path)); err != nil {
		return nil, fmt.Errorf("create state directory: %w", err)
	}

	db, err := openDB(path)
	if err != nil {
		return nil, fmt.Errorf("open state db: %w", err)
	}
	if _, err := db.Exec(`
CREATE TABLE IF NOT EXISTS network_spec_singleton (
	id TEXT PRIMARY KEY,
	spec_json TEXT NOT NULL,
	enabled INTEGER NOT NULL DEFAULT 1,
	updated_at TEXT NOT NULL
)`); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("initialize network spec singleton schema: %w", err)
	}

	return &Store{db: db}, nil
}

func (s *Store) Close() error {
	if s == nil || s.db == nil {
		return nil
	}
	return s.db.Close()
}

func (s *Store) GetSpec() (PersistedSpec, bool, error) {
	var specJSON string
	var enabled int
	err := s.db.QueryRow(`SELECT spec_json, enabled FROM network_spec_singleton WHERE id = ?`, activeSpecKey).Scan(&specJSON, &enabled)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return PersistedSpec{}, false, nil
		}
		return PersistedSpec{}, false, fmt.Errorf("query active network spec: %w", err)
	}

	spec, err := unmarshalSpec(specJSON)
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
		`INSERT INTO network_spec_singleton (id, spec_json, enabled, updated_at)
		 VALUES (?, ?, ?, ?)
		 ON CONFLICT(id) DO UPDATE SET
		 spec_json = excluded.spec_json,
		 enabled = excluded.enabled,
		 updated_at = excluded.updated_at`,
		activeSpecKey,
		string(payload),
		boolToInt(enabled),
		time.Now().UTC().Format(time.RFC3339),
	)
	if err != nil {
		return fmt.Errorf("save network spec: %w", err)
	}
	return nil
}

func (s *Store) DeleteSpec() error {
	if _, err := s.db.Exec(`DELETE FROM network_spec_singleton WHERE id = ?`, activeSpecKey); err != nil {
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

// unmarshalSpec decodes a JSON spec.
func unmarshalSpec(specJSON string) (types.NetworkSpec, error) {
	var spec types.NetworkSpec
	if err := json.Unmarshal([]byte(specJSON), &spec); err != nil {
		return types.NetworkSpec{}, fmt.Errorf("unmarshal network spec: %w", err)
	}
	return spec, nil
}

func boolToInt(b bool) int {
	if b {
		return 1
	}
	return 0
}
