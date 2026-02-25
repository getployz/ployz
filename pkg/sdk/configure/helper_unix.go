//go:build darwin || linux

package configure

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"errors"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
)

const (
	helperSocketDialTimeout = 200 * time.Millisecond
	helperSocketPollWait    = 100 * time.Millisecond
)

func ensureHelperToken(tokenPath string) (string, error) {
	if data, err := os.ReadFile(tokenPath); err == nil {
		token := strings.TrimSpace(string(data))
		if token == "" {
			return "", fmt.Errorf("helper token file is empty: %s", tokenPath)
		}
		return token, nil
	} else if !os.IsNotExist(err) {
		return "", fmt.Errorf("read helper token file: %w", err)
	}

	token, err := generateSocketSecret()
	if err != nil {
		return "", err
	}
	if err := os.MkdirAll(filepath.Dir(tokenPath), 0o700); err != nil {
		return "", fmt.Errorf("create helper token directory: %w", err)
	}
	if err := os.WriteFile(tokenPath, []byte(token+"\n"), 0o600); err != nil {
		return "", fmt.Errorf("write helper token file: %w", err)
	}
	return token, nil
}

func generateSocketSecret() (string, error) {
	buf := make([]byte, 32)
	if _, err := rand.Read(buf); err != nil {
		return "", fmt.Errorf("generate helper secret: %w", err)
	}
	return base64.RawURLEncoding.EncodeToString(buf), nil
}

func waitForHelperSocket(ctx context.Context, socketPath string, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	for {
		if _, err := os.Stat(socketPath); err == nil {
			conn, dialErr := net.DialTimeout("unix", socketPath, helperSocketDialTimeout)
			if dialErr == nil {
				_ = conn.Close()
				return nil
			}
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("privileged helper socket did not become ready at %s", socketPath)
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(helperSocketPollWait):
		}
	}
}

func removeStaleSocketPath(rawPath string) error {
	path := filepath.Clean(strings.TrimSpace(rawPath))
	if path == "." {
		return fmt.Errorf("invalid socket path")
	}
	if err := os.Remove(path); err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	return nil
}

func resolvePloyzBinary() (string, error) {
	if path, err := exec.LookPath("ployz"); err == nil {
		return path, nil
	}
	const defaultInstalledPloyz = "/usr/local/bin/ployz"
	if st, err := os.Stat(defaultInstalledPloyz); err == nil && !st.IsDir() {
		return defaultInstalledPloyz, nil
	}
	exe, err := os.Executable()
	if err != nil {
		return "", err
	}
	if strings.TrimSpace(exe) == "" {
		return "", fmt.Errorf("empty executable path")
	}
	if isGoRunExecutablePath(exe) {
		return "", fmt.Errorf("ployz not found in PATH and current executable is a temporary go run binary (%s); run `just install` and retry", exe)
	}
	return exe, nil
}

func isGoRunExecutablePath(path string) bool {
	trimmed := strings.TrimSpace(path)
	if trimmed == "" {
		return false
	}
	return strings.Contains(filepath.Clean(trimmed), string(filepath.Separator)+"go-build")
}

func dialUnixSocket(socketPath string) (net.Conn, error) {
	return net.DialTimeout("unix", socketPath, helperSocketDialTimeout)
}
