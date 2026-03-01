package daemon

import (
	"context"
	"log/slog"

	"ployz/machine"

	systemd "github.com/coreos/go-systemd/daemon"
	"golang.org/x/sync/errgroup"
)

// Run starts the machine, gRPC server, and systemd notification, then
// blocks until ctx is cancelled.
func Run(ctx context.Context, m *machine.Machine, socketPath string) error {
	srv := NewServer(m)

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
