package api

import (
	"errors"
	"fmt"
	"net"
	"os"
	"os/user"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
)

// --- Utilities ---

func listenUnix(socketPath string) (net.Listener, error) {
	if err := os.MkdirAll(filepath.Dir(socketPath), 0o755); err != nil {
		return nil, fmt.Errorf("create socket directory: %w", err)
	}
	if err := os.Remove(socketPath); err != nil && !errors.Is(err, os.ErrNotExist) {
		return nil, fmt.Errorf("remove stale socket: %w", err)
	}

	ln, err := net.Listen("unix", socketPath)
	if err != nil {
		return nil, fmt.Errorf("listen unix: %w", err)
	}
	if err := os.Chmod(socketPath, 0o660); err != nil {
		_ = ln.Close() // best-effort cleanup
		return nil, fmt.Errorf("set socket permissions: %w", err)
	}
	if err := ensureSocketGroup(socketPath); err != nil {
		_ = ln.Close() // best-effort cleanup
		return nil, err
	}
	return ln, nil
}

func internalSocketPath(externalPath string) string {
	dir := filepath.Dir(externalPath)
	base := filepath.Base(externalPath)
	ext := filepath.Ext(base)
	name := strings.TrimSuffix(base, ext)
	return filepath.Join(dir, name+"-internal"+ext)
}

func ensureSocketGroup(socketPath string) error {
	switch runtime.GOOS {
	case "darwin":
		if err := os.Chmod(socketPath, 0o666); err != nil {
			if errors.Is(err, os.ErrPermission) {
				return nil
			}
			return fmt.Errorf("set daemon socket permissions: %w", err)
		}
		return nil
	case "linux":
		group, err := user.LookupGroup("ployz")
		if err != nil {
			return nil
		}
		gid, err := strconv.Atoi(group.Gid)
		if err != nil {
			return nil
		}
		if err := os.Chown(socketPath, -1, gid); err != nil {
			if errors.Is(err, os.ErrPermission) {
				return nil
			}
			return fmt.Errorf("set daemon socket group: %w", err)
		}
		return nil
	default:
		return nil
	}
}
