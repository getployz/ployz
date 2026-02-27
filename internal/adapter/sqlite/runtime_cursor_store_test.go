package sqlite

import (
	"context"
	"path/filepath"
	"testing"
	"time"
)

func TestRuntimeCursorStoreSetAndGet(t *testing.T) {
	t.Parallel()

	ctx := context.Background()
	store := RuntimeCursorStore{}
	dataDir := filepath.Join(t.TempDir(), "default")

	value, ok, err := store.GetCursor(ctx, dataDir, "docker-events")
	if err != nil {
		t.Fatalf("get missing cursor: %v", err)
	}
	if ok {
		t.Fatalf("expected missing cursor")
	}
	if value != "" {
		t.Fatalf("expected empty value for missing cursor, got %q", value)
	}

	if err := store.SetCursor(ctx, dataDir, "docker-events", "12345", time.Date(2026, 2, 26, 13, 0, 0, 0, time.UTC)); err != nil {
		t.Fatalf("set cursor: %v", err)
	}

	value, ok, err = store.GetCursor(ctx, dataDir, "docker-events")
	if err != nil {
		t.Fatalf("get cursor: %v", err)
	}
	if !ok {
		t.Fatalf("expected cursor to exist")
	}
	if value != "12345" {
		t.Fatalf("expected cursor value 12345, got %q", value)
	}

	if err := store.SetCursor(ctx, dataDir, "docker-events", "12346", time.Date(2026, 2, 26, 13, 1, 0, 0, time.UTC)); err != nil {
		t.Fatalf("update cursor: %v", err)
	}
	value, ok, err = store.GetCursor(ctx, dataDir, "docker-events")
	if err != nil {
		t.Fatalf("get updated cursor: %v", err)
	}
	if !ok || value != "12346" {
		t.Fatalf("expected updated cursor 12346, got ok=%v value=%q", ok, value)
	}
}

func TestRuntimeCursorStoreValidationErrors(t *testing.T) {
	t.Parallel()

	ctx := context.Background()
	store := RuntimeCursorStore{}
	dataDir := filepath.Join(t.TempDir(), "default")

	if _, _, err := store.GetCursor(ctx, "", "docker-events"); err == nil {
		t.Fatalf("expected error for empty data dir")
	}
	if _, _, err := store.GetCursor(ctx, dataDir, ""); err == nil {
		t.Fatalf("expected error for empty cursor name")
	}
	if err := store.SetCursor(ctx, "", "docker-events", "1", time.Time{}); err == nil {
		t.Fatalf("expected error for empty data dir on set")
	}
	if err := store.SetCursor(ctx, dataDir, "", "1", time.Time{}); err == nil {
		t.Fatalf("expected error for empty cursor name on set")
	}
}
