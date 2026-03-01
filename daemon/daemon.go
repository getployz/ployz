package daemon

import (
	"context"
	"fmt"
	"log/slog"

	"ployz/machine"

	systemd "github.com/coreos/go-systemd/daemon"
	"golang.org/x/sync/errgroup"
)

// Run starts the machine, gRPC server, and systemd notification, then
// blocks until ctx is cancelled. If a saved network config exists and a
// builder is available, the mesh is built and attached before starting.
func Run(ctx context.Context, m *machine.Machine, buildMesh machine.MeshBuilder, socketPath string) error {
	hasConfig, err := m.HasNetworkConfig()
	if err != nil {
		return fmt.Errorf("check network config: %w", err)
	}
	if hasConfig {
		if buildMesh == nil {
			return fmt.Errorf("saved network config exists but no mesh builder available")
		}
		slog.Info("Restoring mesh from saved config.")
		ns, err := buildMesh(ctx)
		if err != nil {
			return fmt.Errorf("build mesh: %w", err)
		}
		m.SetMesh(ns)
	}

	srv := NewServer(m, buildMesh)

	g, ctx := errgroup.WithContext(ctx)
	g.Go(func() error {
		slog.Info("Starting machine.")

		// Notify systemd that the daemon is ready when the machine is started.
		go func() {
			select {
			case <-m.Started():
				_, err := systemd.SdNotify(false, systemd.SdNotifyReady)
				if err != nil {
					slog.Error("Failed to notify systemd that the daemon is ready.", "err", err)
				}
			case <-ctx.Done():
			}
		}()

		return m.Run(ctx)
	})
	g.Go(func() error { return srv.ListenAndServe(ctx, socketPath) })
	return g.Wait()
}
