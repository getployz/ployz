package agent

import (
	"context"
	"fmt"
	"path/filepath"
	"strings"
	"time"

	"ployz/pkg/sdk/client"
)

type InstallConfig struct {
	DataRoot   string
	SocketPath string
}

type ServiceStatus struct {
	DaemonInstalled bool
	DaemonRunning   bool
	Platform        string
}

type PlatformService interface {
	Install(ctx context.Context, cfg InstallConfig) error
	Uninstall(ctx context.Context) error
	Status(ctx context.Context) (ServiceStatus, error)
}

func DaemonLogPath(dataRoot string) string {
	return filepath.Join(dataRoot, "ployzd.log")
}

func HealthCheck(ctx context.Context, socketPath string) error {
	checkCtx, cancel := context.WithTimeout(ctx, 2*time.Second)
	defer cancel()

	api, err := client.NewUnix(socketPath)
	if err != nil {
		return fmt.Errorf("connect to daemon: %w", err)
	}
	defer func() { _ = api.Close() }()

	if _, err := api.GetStatus(checkCtx); err != nil {
		return fmt.Errorf("daemon health check: %w", err)
	}
	return nil
}

func WaitReady(ctx context.Context, socketPath string, timeout time.Duration) error {
	deadline, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	var lastErr error
	for {
		if err := HealthCheck(deadline, socketPath); err == nil {
			return nil
		} else {
			lastErr = err
			if isPermanentHealthError(err) {
				return err
			}
		}
		select {
		case <-deadline.Done():
			if lastErr != nil {
				return fmt.Errorf("agent did not become ready within %s: %w", timeout, lastErr)
			}
			return fmt.Errorf("agent did not become ready within %s", timeout)
		case <-time.After(300 * time.Millisecond):
		}
	}
}

func isPermanentHealthError(err error) bool {
	if err == nil {
		return false
	}
	msg := strings.ToLower(err.Error())
	return strings.Contains(msg, "permission denied") || strings.Contains(msg, "operation not permitted")
}
