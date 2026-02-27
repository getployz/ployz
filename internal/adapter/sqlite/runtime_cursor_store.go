package sqlite

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"strings"
	"time"

	"ployz/internal/observed"
)

// RuntimeCursorStore persists runtime stream/reconcile cursors in local SQLite.
type RuntimeCursorStore struct{}

var _ observed.SyncCursorStore = RuntimeCursorStore{}

func (RuntimeCursorStore) GetCursor(ctx context.Context, dataDir, name string) (string, bool, error) {
	if strings.TrimSpace(dataDir) == "" {
		return "", false, fmt.Errorf("data dir is required")
	}
	name = strings.TrimSpace(name)
	if name == "" {
		return "", false, fmt.Errorf("cursor name is required")
	}

	db, err := openMachineDB(machineDBPath(dataDir))
	if err != nil {
		return "", false, fmt.Errorf("open machine db: %w", err)
	}
	defer db.Close()

	if err := ensureRuntimeCursorSchema(db); err != nil {
		return "", false, err
	}

	var value string
	if err := db.QueryRowContext(ctx, `SELECT value FROM runtime_sync_cursors WHERE name = ?`, name).Scan(&value); err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return "", false, nil
		}
		return "", false, fmt.Errorf("query runtime cursor %q: %w", name, err)
	}
	return value, true, nil
}

func (RuntimeCursorStore) SetCursor(ctx context.Context, dataDir, name, value string, updatedAt time.Time) error {
	if strings.TrimSpace(dataDir) == "" {
		return fmt.Errorf("data dir is required")
	}
	name = strings.TrimSpace(name)
	if name == "" {
		return fmt.Errorf("cursor name is required")
	}

	db, err := openMachineDB(machineDBPath(dataDir))
	if err != nil {
		return fmt.Errorf("open machine db: %w", err)
	}
	defer db.Close()

	if err := ensureRuntimeCursorSchema(db); err != nil {
		return err
	}

	if updatedAt.IsZero() {
		updatedAt = time.Now().UTC()
	}
	if _, err := db.ExecContext(
		ctx,
		`INSERT INTO runtime_sync_cursors (name, value, updated_at) VALUES (?, ?, ?)
ON CONFLICT(name) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at`,
		name,
		value,
		updatedAt.UTC().Format(time.RFC3339Nano),
	); err != nil {
		return fmt.Errorf("upsert runtime cursor %q: %w", name, err)
	}

	return nil
}

func ensureRuntimeCursorSchema(db *sql.DB) error {
	const schema = `
CREATE TABLE IF NOT EXISTS runtime_sync_cursors (
	name TEXT PRIMARY KEY,
	value TEXT NOT NULL,
	updated_at TEXT NOT NULL
);`
	if _, err := db.Exec(schema); err != nil {
		return fmt.Errorf("initialize runtime cursor schema: %w", err)
	}
	return nil
}
