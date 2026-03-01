package daemon

import (
	"context"
	"fmt"
	"log/slog"

	"ployz/machine"

	systemd "github.com/coreos/go-systemd/daemon"
	"golang.org/x/sync/errgroup"
)

type Daemon struct {
	machine *machine.Machine
}

func New(dataDir string) (*Daemon, error) {
	m, err := machine.New(dataDir)
	if err != nil {
		return nil, err
	}

	return &Daemon{machine: m}, nil
}

func (d *Daemon) run(ctx context.Context) error {
	slog.Info("Starting machine.")

	// Notify systemd that the daemon is ready when the machine is started.
	go func() {
		select {
		case <-d.machine.Started():
			_, err := systemd.SdNotify(false, systemd.SdNotifyReady)
			if err != nil {
				slog.Error("Failed to notify systemd that the daemon is ready.", "err", err)
			}
		case <-ctx.Done():
		}
	}()

	return d.machine.Run(ctx)
}

// Run creates a daemon and gRPC server, then runs both until ctx is cancelled.
func Run(ctx context.Context, dataRoot, socketPath string) error {
	d, err := New(dataRoot)
	if err != nil {
		return fmt.Errorf("create daemon: %w", err)
	}

	srv := NewServer(d.machine)

	g, ctx := errgroup.WithContext(ctx)
	g.Go(func() error { return d.run(ctx) })
	g.Go(func() error { return srv.ListenAndServe(ctx, socketPath) })
	return g.Wait()
}
