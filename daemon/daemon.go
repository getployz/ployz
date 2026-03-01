package daemon

import (
	"context"
	"log/slog"

	"ployz"
	"ployz/machine"

	systemd "github.com/coreos/go-systemd/daemon"
	"golang.org/x/sync/errgroup"
)

// Daemon is the runtime shell around a Machine. It manages the gRPC
// server and systemd notifications. Construct the machine in cmd/ via
// platform.NewMachine, then hand it here.
type Daemon struct {
	machine *machine.Machine
}

// New wraps an existing machine in a daemon.
func New(m *machine.Machine) *Daemon {
	return &Daemon{machine: m}
}

// Status delegates to the machine.
func (d *Daemon) Status() ployz.Machine {
	return d.machine.Status()
}

// InitNetwork delegates to the machine.
func (d *Daemon) InitNetwork(ctx context.Context, name string) error {
	return d.machine.InitNetwork(ctx, name)
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

// Run starts a daemon and gRPC server, then blocks until ctx is cancelled.
func Run(ctx context.Context, m *machine.Machine, socketPath string) error {
	d := New(m)
	srv := NewServer(d)

	g, ctx := errgroup.WithContext(ctx)
	g.Go(func() error { return d.run(ctx) })
	g.Go(func() error { return srv.ListenAndServe(ctx, socketPath) })
	return g.Wait()
}
