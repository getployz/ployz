package agent

import (
	"context"
	"fmt"
	"time"

	"ployz/cmd/ployz/cmdutil"
)

type InstallConfig struct {
	DataRoot   string
	SocketPath string
}

type ServiceStatus struct {
	DaemonInstalled  bool
	DaemonRunning    bool
	RuntimeInstalled bool
	RuntimeRunning   bool
	Platform         string
}

type PlatformService interface {
	Install(ctx context.Context, cfg InstallConfig) error
	Uninstall(ctx context.Context) error
	Status(ctx context.Context) (ServiceStatus, error)
}

func WaitReady(ctx context.Context, socketPath string, timeout time.Duration) error {
	deadline, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	for {
		if err := cmdutil.HealthCheck(deadline, socketPath); err == nil {
			return nil
		}
		select {
		case <-deadline.Done():
			return fmt.Errorf("agent did not become ready within %s", timeout)
		case <-time.After(300 * time.Millisecond):
		}
	}
}
