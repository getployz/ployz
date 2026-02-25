package agent

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

func removeStaleSocket(socketPath string) error {
	path := filepath.Clean(strings.TrimSpace(socketPath))
	if path == "." {
		return fmt.Errorf("invalid socket path")
	}
	if err := os.Remove(path); err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	return nil
}
