package convergence

import (
	"context"
	"fmt"
	"log/slog"
	"net/netip"
	"sync"
	"time"

	"ployz/internal/support/check"
	"ployz/internal/daemon/membership"
	"ployz/internal/daemon/overlay"
	"ployz/pkg/sdk/defaults"
)

const (
	// fullSyncInterval is 30s: long enough to batch changes, short enough to catch missed events.
	fullSyncInterval = 30 * time.Second
	// heartbeatInterval is 1s: frequent enough for sub-second freshness tracking.
	heartbeatInterval = 1 * time.Second
	// pingInterval is 1s: matches heartbeat cadence for consistent health reporting.
	pingInterval = 1 * time.Second
	// heartbeatSubscribeMaxRetries is 3: heartbeat is non-critical, fail fast and degrade gracefully.
	heartbeatSubscribeMaxRetries = 3
	// maxMachineSubscribeRetries is 30: ~30s of retries before giving up on machine subscription.
	maxMachineSubscribeRetries = 30
	// maxHeartbeatBumpFailures is 10: consecutive heartbeat bump failures before logging a warning.
	maxHeartbeatBumpFailures = 10
)

type Supervisor struct {
	Spec           overlay.Config
	Registry       Registry       // injected: Corrosion machine/heartbeat store
	PeerReconciler PeerReconciler // injected: applies peer configuration
	StateStore     overlay.StateStore
	Broker         *Broker
	Freshness      *FreshnessTracker
	NTP            *Checker
	Ping           *PingTracker
	Clock          overlay.Clock
	OnEvent        func(eventType, message string)
	OnFailure      func(error)
}

func (w *Supervisor) getClock() overlay.Clock {
	if w.Clock != nil {
		return w.Clock
	}
	return overlay.RealClock{}
}

func (w *Supervisor) emit(eventType, message string) {
	if w.OnEvent != nil {
		w.OnEvent(eventType, message)
	}
	slog.Debug("supervisor event", "event", eventType, "message", message)
}

func (w *Supervisor) fail(err error) {
	if w.OnFailure != nil {
		w.OnFailure(err)
	}
	if err != nil {
		slog.Warn("supervisor failure", "err", err)
	}
}

func (w *Supervisor) syncPeersAndReport(ctx context.Context, cfg overlay.Config, machines []membership.MachineRow) {
	count, err := w.PeerReconciler.ReconcilePeers(ctx, cfg, machines)
	if err != nil {
		w.emit("supervisor.sync.error", err.Error())
		w.fail(err)
		return
	}
	w.emit("supervisor.sync.success", fmt.Sprintf("synchronized %d peers", count))
}

func (w *Supervisor) refreshAndSync(ctx context.Context, cfg overlay.Config) ([]membership.MachineRow, bool) {
	snap, err := w.Registry.ListMachineRows(ctx)
	if err != nil {
		w.emit("supervisor.sync.error", err.Error())
		w.fail(err)
		return nil, false
	}
	w.syncPeersAndReport(ctx, cfg, snap)
	return snap, true
}

func (w *Supervisor) hydrateHeartbeats(rows []membership.HeartbeatRow) {
	for _, hb := range rows {
		if t, err := time.Parse(time.RFC3339Nano, hb.UpdatedAt); err == nil {
			w.Freshness.RecordSeen(hb.NodeID, t)
		}
	}
}

func (w *Supervisor) Run(ctx context.Context) error {
	check.Assert(w.Registry != nil, "Supervisor.Run: Registry must not be nil")
	check.Assert(w.PeerReconciler != nil, "Supervisor.Run: PeerReconciler must not be nil")

	cfg, err := overlay.NormalizeConfig(w.Spec)
	if err != nil {
		return err
	}

	if err := w.Registry.EnsureMachineTable(ctx); err != nil {
		return err
	}
	if err := w.Registry.EnsureHeartbeatTable(ctx); err != nil {
		return err
	}
	if w.Broker == nil {
		w.Broker = NewBroker(w.Registry)
	}

	// Load persisted state and register the local machine.
	selfID := ""
	if w.StateStore != nil {
		if st, err := overlay.LoadState(w.StateStore, cfg); err == nil {
			selfID = st.WGPublic
			if selfID != "" {
				if err := w.Registry.UpsertMachine(ctx, overlay.MachineRow{
					ID:           st.WGPublic,
					PublicKey:    st.WGPublic,
					Subnet:       st.Subnet,
					ManagementIP: st.Management,
					Endpoint:     st.Advertise,
					UpdatedAt:    w.getClock().Now().UTC().Format(time.RFC3339),
				}, 0); err != nil {
					return fmt.Errorf("register local machine: %w", err)
				}
				w.emit("self.registered", fmt.Sprintf("registered local machine %s", selfID[:8]))
			}
		}
	}

	// Start heartbeat writer goroutine.
	if selfID != "" {
		go runHeartbeat(ctx, w.Registry, selfID, w.getClock())
	}

	// Start NTP checker goroutine.
	if w.NTP != nil {
		go w.NTP.Run(ctx)
	}

	// Subscribe to machines.
	machines, machCh, err := w.subscribeMachinesWithRetry(ctx)
	if err != nil {
		return err
	}
	w.emit("subscribe.ready", fmt.Sprintf("machine subscription snapshot size %d", len(machines)))
	w.syncPeersAndReport(ctx, cfg, machines)

	// Mutex-protected machines snapshot for the ping goroutine.
	var machinesMu sync.RWMutex
	setMachines := func(m []membership.MachineRow) {
		machinesMu.Lock()
		machines = m
		machinesMu.Unlock()
	}
	getMachinesSnapshot := func() []membership.MachineRow {
		machinesMu.RLock()
		snap := make([]membership.MachineRow, len(machines))
		copy(snap, machines)
		machinesMu.RUnlock()
		return snap
	}

	// Start ping tracker goroutine.
	if w.Ping != nil && selfID != "" {
		go w.Ping.Run(ctx, selfID, pingInterval, func() map[string]string {
			return resolvePingAddrs(getMachinesSnapshot(), cfg.Network)
		})
	}

	// Subscribe to heartbeats for freshness tracking.
	var heartbeatChanges <-chan membership.HeartbeatChange
	if w.Freshness != nil {
		heartbeatSnapshot, ch, hbErr := w.subscribeHeartbeatsWithRetry(ctx)
		if hbErr == nil {
			heartbeatChanges = ch
			w.hydrateHeartbeats(heartbeatSnapshot)
		}
	}

	ticker := time.NewTicker(fullSyncInterval)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case change, ok := <-machCh:
			if !ok {
				m, ch, subErr := w.subscribeMachinesWithRetry(ctx)
				if subErr != nil {
					return subErr
				}
				machCh = ch
				setMachines(m)
				w.emit("subscribe.ready", fmt.Sprintf("machine subscription restored (%d machines)", len(m)))
				w.syncPeersAndReport(ctx, cfg, m)
				continue
			}

			if change.Kind == membership.ChangeResync {
				w.emit("subscribe.resync", "machine subscription resynced")
				if snap, ok := w.refreshAndSync(ctx, cfg); ok {
					setMachines(snap)
				}
				continue
			}
			updated := applyMachineChange(getMachinesSnapshot(), change)
			setMachines(updated)
			w.syncPeersAndReport(ctx, cfg, updated)
		case hbChange, ok := <-heartbeatChanges:
			if !ok {
				// Heartbeat subscription closed — try to restore.
				snapshot, ch, hbErr := w.subscribeHeartbeatsWithRetry(ctx)
				if hbErr == nil {
					heartbeatChanges = ch
					w.hydrateHeartbeats(snapshot)
				} else {
					heartbeatChanges = nil
				}
				continue
			}
			if w.Freshness == nil {
				continue
			}
			switch hbChange.Kind {
			case membership.ChangeDeleted:
				w.Freshness.Remove(hbChange.Heartbeat.NodeID)
			case membership.ChangeResync:
				// Nothing to do — next full sync will re-sync.
			default:
				if t, err := time.Parse(time.RFC3339Nano, hbChange.Heartbeat.UpdatedAt); err == nil {
					w.Freshness.RecordSeen(hbChange.Heartbeat.NodeID, t)
				}
			}
		case <-ticker.C:
			if snap, ok := w.refreshAndSync(ctx, cfg); ok {
				setMachines(snap)
			}
		}
	}
}

func runHeartbeat(ctx context.Context, reg Registry, nodeID string, clock overlay.Clock) {
	ticker := time.NewTicker(heartbeatInterval)
	defer ticker.Stop()

	var consecutiveFailures int
	for {
		now := clock.Now().UTC().Format(time.RFC3339Nano)
		if err := reg.BumpHeartbeat(ctx, nodeID, now); err != nil {
			consecutiveFailures++
			if consecutiveFailures == maxHeartbeatBumpFailures {
				slog.Warn("heartbeat bump failing repeatedly", "failures", consecutiveFailures, "err", err)
			}
		} else {
			consecutiveFailures = 0
		}

		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
	}
}

func applyMachineChange(machines []membership.MachineRow, change membership.MachineChange) []membership.MachineRow {
	switch change.Kind {
	case membership.ChangeAdded, membership.ChangeUpdated:
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
	case membership.ChangeDeleted:
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
func resolvePingAddrs(machines []membership.MachineRow, networkName string) map[string]string {
	port := defaults.DaemonAPIPort(networkName)
	out := make(map[string]string, len(machines))
	for _, m := range machines {
		if m.Subnet == "" {
			continue
		}
		prefix, err := netip.ParsePrefix(m.Subnet)
		if err != nil {
			continue
		}
		out[m.ID] = fmt.Sprintf("%s:%d", membership.MachineIP(prefix), port)
	}
	return out
}

func (w *Supervisor) subscribeMachinesWithRetry(ctx context.Context) ([]membership.MachineRow, <-chan membership.MachineChange, error) {
	if w.Broker != nil {
		for range maxMachineSubscribeRetries {
			machines, changes, err := w.Broker.SubscribeMachines(ctx)
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
		return nil, nil, fmt.Errorf("machine subscription failed after %d retries", maxMachineSubscribeRetries)
	}

	for range maxMachineSubscribeRetries {
		if err := w.Registry.EnsureMachineTable(ctx); err != nil {
			select {
			case <-ctx.Done():
				return nil, nil, ctx.Err()
			case <-time.After(time.Second):
				continue
			}
		}
		machines, changes, err := w.Registry.SubscribeMachines(ctx)
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
	return nil, nil, fmt.Errorf("machine subscription failed after %d retries", maxMachineSubscribeRetries)
}

func (w *Supervisor) subscribeHeartbeatsWithRetry(ctx context.Context) ([]membership.HeartbeatRow, <-chan membership.HeartbeatChange, error) {
	if w.Broker != nil {
		for range heartbeatSubscribeMaxRetries {
			hbs, changes, err := w.Broker.SubscribeHeartbeats(ctx)
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

	for range heartbeatSubscribeMaxRetries {
		if err := w.Registry.EnsureHeartbeatTable(ctx); err != nil {
			select {
			case <-ctx.Done():
				return nil, nil, ctx.Err()
			case <-time.After(time.Second):
				continue
			}
		}
		hbs, changes, err := w.Registry.SubscribeHeartbeats(ctx)
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
