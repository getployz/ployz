package daemon

import (
	"context"
	"log/slog"

	"ployz/machine"

	systemd "github.com/coreos/go-systemd/daemon"
)

type Daemon struct {
	machine *machine.Machine
}

func New(dataDir string) (*Daemon, error) {
	m, err := machine.NewProduction(dataDir)
	if err != nil {
		return nil, err
	}

	return &Daemon{machine: m}, nil
}

func (d *Daemon) Run(ctx context.Context) error {
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
