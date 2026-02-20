package reconcile

import (
	"context"
	"fmt"
	"time"

	"ployz/internal/machine"
	"ployz/internal/machine/registry"
)

type Worker struct {
	Spec      machine.Config
	OnEvent   func(eventType, message string)
	OnFailure func(error)
}

func (w *Worker) emit(eventType, message string) {
	if w.OnEvent != nil {
		w.OnEvent(eventType, message)
	}
}

func (w *Worker) fail(err error) {
	if w.OnFailure != nil {
		w.OnFailure(err)
	}
}

func (w *Worker) Run(ctx context.Context) error {
	cfg, err := machine.NormalizeConfig(w.Spec)
	if err != nil {
		return err
	}

	ctrl, err := machine.New()
	if err != nil {
		return err
	}
	defer ctrl.Close()

	reg := registry.New(cfg.CorrosionAPIAddr)
	if err := reg.EnsureMachineTable(ctx); err != nil {
		return err
	}

	machines, machCh, err := w.subscribeMachinesWithRetry(ctx, reg, cfg)
	if err != nil {
		return err
	}
	w.emit("subscribe.ready", fmt.Sprintf("machine subscription snapshot size %d", len(machines)))
	if count, rErr := ctrl.ReconcilePeers(ctx, cfg, machines); rErr != nil {
		w.emit("reconcile.error", rErr.Error())
		w.fail(rErr)
	} else {
		w.emit("reconcile.success", fmt.Sprintf("reconciled %d peers", count))
	}

	ticker := time.NewTicker(30 * time.Second)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case change, ok := <-machCh:
			if !ok {
				machines, machCh, err = w.subscribeMachinesWithRetry(ctx, reg, cfg)
				if err != nil {
					return err
				}
				w.emit("subscribe.ready", fmt.Sprintf("machine subscription restored (%d machines)", len(machines)))
				if count, rErr := ctrl.ReconcilePeers(ctx, cfg, machines); rErr != nil {
					w.emit("reconcile.error", rErr.Error())
					w.fail(rErr)
				} else {
					w.emit("reconcile.success", fmt.Sprintf("reconciled %d peers", count))
				}
				continue
			}

			if change.Kind == registry.ChangeResync {
				w.emit("subscribe.resync", "machine subscription resynced")
				snap, snapErr := reg.ListMachines(ctx, cfg.Network)
				if snapErr != nil {
					w.emit("reconcile.error", snapErr.Error())
					w.fail(snapErr)
					continue
				}
				machines = snap
			} else {
				machines = applyMachineChange(machines, change)
			}
			if count, rErr := ctrl.ReconcilePeers(ctx, cfg, machines); rErr != nil {
				w.emit("reconcile.error", rErr.Error())
				w.fail(rErr)
			} else {
				w.emit("reconcile.success", fmt.Sprintf("reconciled %d peers", count))
			}
		case <-ticker.C:
			snap, snapErr := reg.ListMachines(ctx, cfg.Network)
			if snapErr != nil {
				w.emit("reconcile.error", snapErr.Error())
				w.fail(snapErr)
				continue
			}
			machines = snap
			if count, rErr := ctrl.ReconcilePeers(ctx, cfg, machines); rErr != nil {
				w.emit("reconcile.error", rErr.Error())
				w.fail(rErr)
			} else {
				w.emit("reconcile.success", fmt.Sprintf("reconciled %d peers", count))
			}
		}
	}
}

func applyMachineChange(machines []registry.MachineRow, change registry.MachineChange) []registry.MachineRow {
	switch change.Kind {
	case registry.ChangeAdded, registry.ChangeUpdated:
		replaced := false
		for i := range machines {
			if machines[i].ID == change.Machine.ID {
				machines[i] = change.Machine
				replaced = true
				break
			}
		}
		if !replaced {
			machines = append(machines, change.Machine)
		}
	case registry.ChangeDeleted:
		out := machines[:0]
		for _, m := range machines {
			if change.Machine.ID != "" && m.ID == change.Machine.ID {
				continue
			}
			if change.Machine.PublicKey != "" && m.PublicKey == change.Machine.PublicKey {
				continue
			}
			out = append(out, m)
		}
		machines = out
	}
	return machines
}

func (w *Worker) subscribeMachinesWithRetry(
	ctx context.Context,
	reg registry.Store,
	cfg machine.Config,
) ([]registry.MachineRow, <-chan registry.MachineChange, error) {
	for {
		if err := reg.EnsureMachineTable(ctx); err != nil {
			select {
			case <-ctx.Done():
				return nil, nil, ctx.Err()
			case <-time.After(time.Second):
				continue
			}
		}
		machines, changes, err := reg.SubscribeMachines(ctx, cfg.Network)
		if err == nil {
			return machines, changes, nil
		}
		w.emit("subscribe.error", err.Error())
		select {
		case <-ctx.Done():
			return nil, nil, ctx.Err()
		case <-time.After(time.Second):
		}
	}
}
