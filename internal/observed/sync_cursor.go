package observed

import (
	"context"
	"time"
)

// SyncCursorStore persists last-processed cursors for runtime observation streams.
type SyncCursorStore interface {
	GetCursor(ctx context.Context, dataDir, name string) (value string, ok bool, err error)
	SetCursor(ctx context.Context, dataDir, name, value string, updatedAt time.Time) error
}
