package logging

import (
	"fmt"
	"log/slog"
	"os"
	"strings"
)

const (
	LevelDebug = "debug"
	LevelInfo  = "info"
	LevelWarn  = "warn"
	LevelError = "error"
)

// Configure installs a process-wide slog default logger.
//
// Supported levels: debug, info, warn, error.
func Configure(level string) error {
	parsed, err := parseLevel(level)
	if err != nil {
		return err
	}

	h := slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: parsed})
	slog.SetDefault(slog.New(h))
	return nil
}

func parseLevel(level string) (slog.Level, error) {
	switch strings.ToLower(strings.TrimSpace(level)) {
	case "", LevelInfo:
		return slog.LevelInfo, nil
	case LevelDebug:
		return slog.LevelDebug, nil
	case LevelWarn:
		return slog.LevelWarn, nil
	case LevelError:
		return slog.LevelError, nil
	default:
		return 0, fmt.Errorf("invalid log level %q", level)
	}
}
