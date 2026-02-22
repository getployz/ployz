package reconcile

import (
	"context"
	"fmt"
	"log/slog"
	"net/netip"
	"sync"
	"time"

	"ployz/internal/network"
	"ployz/pkg/sdk/defaults"
)

type Worker struct {
	Spec           network.Config
	Registry       Registry       // injected: Corrosion machine/heartbeat store
	PeerReconciler PeerReconciler // injected: applies peer configuration
	Freshness      *FreshnessTracker
	NTP            *NTPChecker
	Ping           *PingTracker
	OnEvent        func(eventType, message string)
	OnFailure      func(error)
}

func (w *Worker) emit(eventType, message string) {
	if w.OnEvent != nil {
		w.OnEvent(eventType, message)
	}
	slog.Debug("reconcile event", "event", eventType, "message", message)
}

func (w *Worker) fail(err error) {
	if w.OnFailure != nil {
		w.OnFailure(err)
	}
	if err != nil {
		slog.Warn("reconcile failure", "err", err)
	}
}

func (w *Worker) reconcileAndReport(ctx context.Context, cfg network.Config, machines []network.MachineRow) {
	count, err := w.PeerReconciler.ReconcilePeers(ctx, cfg, machines)
	if err != nil {
		w.emit("reconcile.error", err.Error())
		w.fail(err)
		return
	}
	w.emit("reconcile.success", fmt.Sprintf("reconciled %d peers", count))
}

func (w *Worker) refreshAndReconcile(ctx context.Context, cfg network.Config) ([]network.MachineRow, bool) {
	snap, err := w.Registry.ListMachineRows(ctx)
	if err != nil {
		w.emit("reconcile.error", err.Error())
		w.fail(err)
		return nil, false
	}
	w.reconcileAndReport(ctx, cfg, snap)
	return snap, true
}

func (w *Worker) Run(ctx context.Context) error {
	cfg, err := network.NormalizeConfig(w.Spec)
	if err != nil {
		return err
	}

	reg := w.Registry
	if err := reg.EnsureMachineTable(ctx); err != nil {
		return err
	}
	if err := reg.EnsureHeartbeatTable(ctx); err != nil {
		return err
	}

	// Determine self ID from WireGuard public key.
	selfID := ""
	if st, err := network.LoadState(cfg); err == nil {
		selfID = st.WGPublic
	}

	// Start heartbeat writer goroutine.
	if selfID != "" {
		go runHeartbeat(ctx, reg, selfID)
	}

	// Start NTP checker goroutine.
	if w.NTP != nil {
		go w.NTP.Run(ctx)
	}

	// Subscribe to machines.
	machines, machCh, err := w.subscribeMachinesWithRetry(ctx, reg)
	if err != nil {
		return err
	}
	w.emit("subscribe.ready", fmt.Sprintf("machine subscription snapshot size %d", len(machines)))
	w.reconcileAndReport(ctx, cfg, machines)

	// Mutex-protected machines snapshot for the ping goroutine.
	var machinesMu sync.RWMutex
	setMachines := func(m []network.MachineRow) {
		machinesMu.Lock()
		machines = m
		machinesMu.Unlock()
	}
	getMachinesSnapshot := func() []network.MachineRow {
		machinesMu.RLock()
		snap := make([]network.MachineRow, len(machines))
		copy(snap, machines)
		machinesMu.RUnlock()
		return snap
	}

	// Start ping tracker goroutine.
	if w.Ping != nil && selfID != "" {
		go w.Ping.Run(ctx, selfID, 1*time.Second, func() map[string]string {
			return resolvePingAddrs(getMachinesSnapshot(), cfg.Network)
		})
	}

	// Subscribe to heartbeats for freshness tracking.
	var hbCh <-chan network.HeartbeatChange
	if w.Freshness != nil {
		hbSnap, ch, hbErr := w.subscribeHeartbeatsWithRetry(ctx, reg)
		if hbErr == nil {
			hbCh = ch
			for _, hb := range hbSnap {
				if t, pErr := time.Parse(time.RFC3339Nano, hb.UpdatedAt); pErr == nil {
					w.Freshness.RecordSeen(hb.NodeID, t)
				}
			}
		}
	}

	ticker := time.NewTicker(30 * time.Second)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case change, ok := <-machCh:
			if !ok {
				m, ch, subErr := w.subscribeMachinesWithRetry(ctx, reg)
				if subErr != nil {
					return subErr
				}
				machCh = ch
				setMachines(m)
				w.emit("subscribe.ready", fmt.Sprintf("machine subscription restored (%d machines)", len(m)))
				w.reconcileAndReport(ctx, cfg, m)
				continue
			}

			if change.Kind == network.ChangeResync {
				w.emit("subscribe.resync", "machine subscription resynced")
				if snap, ok := w.refreshAndReconcile(ctx, cfg); ok {
					setMachines(snap)
				}
				continue
			}
			setMachines(applyMachineChange(getMachinesSnapshot(), change))
			w.reconcileAndReport(ctx, cfg, getMachinesSnapshot())
		case hbChange, ok := <-hbCh:
			if !ok {
				// Heartbeat subscription closed — try to restore.
				hbSnap, ch, hbErr := w.subscribeHeartbeatsWithRetry(ctx, reg)
				if hbErr == nil {
					hbCh = ch
					for _, hb := range hbSnap {
						if t, pErr := time.Parse(time.RFC3339Nano, hb.UpdatedAt); pErr == nil {
							w.Freshness.RecordSeen(hb.NodeID, t)
						}
					}
				} else {
					hbCh = nil
				}
				continue
			}
			if w.Freshness == nil {
				continue
			}
			if hbChange.Kind == network.ChangeDeleted {
				w.Freshness.Remove(hbChange.Heartbeat.NodeID)
				continue
			}
			if hbChange.Kind == network.ChangeResync {
				continue
			}
			if t, pErr := time.Parse(time.RFC3339Nano, hbChange.Heartbeat.UpdatedAt); pErr == nil {
				w.Freshness.RecordSeen(hbChange.Heartbeat.NodeID, t)
			}
		case <-ticker.C:
			if snap, ok := w.refreshAndReconcile(ctx, cfg); ok {
				setMachines(snap)
			}
		}
	}
}

func runHeartbeat(ctx context.Context, reg Registry, nodeID string) {
	ticker := time.NewTicker(1 * time.Second)
	defer ticker.Stop()

	for {
		now := time.Now().UTC().Format(time.RFC3339Nano)
		_ = reg.BumpHeartbeat(ctx, nodeID, now)

		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
	}
}

func applyMachineChange(machines []network.MachineRow, change network.MachineChange) []network.MachineRow {
	switch change.Kind {
	case network.ChangeAdded, network.ChangeUpdated:
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
	case network.ChangeDeleted:
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

// resolvePingAddrs derives overlay IPv4 + daemon API port for each machine.
func resolvePingAddrs(machines []network.MachineRow, network string) map[string]string {
	port := defaults.DaemonAPIPort(network)
	out := make(map[string]string, len(machines))
	for _, m := range machines {
		if m.Subnet == "" {
			continue
		}
		prefix, err := netip.ParsePrefix(m.Subnet)
		if err != nil {
			continue
		}
		// First host IP in the subnet.
		host := prefix.Masked().Addr().Next()
		out[m.ID] = fmt.Sprintf("%s:%d", host, port)
	}
	return out
}

func (w *Worker) subscribeMachinesWithRetry(
	ctx context.Context,
	reg Registry,
) ([]network.MachineRow, <-chan network.MachineChange, error) {
	for {
		if err := reg.EnsureMachineTable(ctx); err != nil {
			select {
			case <-ctx.Done():
				return nil, nil, ctx.Err()
			case <-time.After(time.Second):
				continue
			}
		}
		machines, changes, err := reg.SubscribeMachines(ctx)
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

func (w *Worker) subscribeHeartbeatsWithRetry(
	ctx context.Context,
	reg Registry,
) ([]network.HeartbeatRow, <-chan network.HeartbeatChange, error) {
	for attempts := 0; attempts < 3; attempts++ {
		if err := reg.EnsureHeartbeatTable(ctx); err != nil {
			select {
			case <-ctx.Done():
				return nil, nil, ctx.Err()
			case <-time.After(time.Second):
				continue
			}
		}
		hbs, changes, err := reg.SubscribeHeartbeats(ctx)
		if err == nil {
			return hbs, changes, nil
		}
		w.emit("subscribe.error", err.Error())
		select {
		case <-ctx.Done():
			return nil, nil, ctx.Err()
		case <-time.After(time.Second):
		}
	}
	return nil, nil, fmt.Errorf("heartbeat subscription failed after retries")
}
