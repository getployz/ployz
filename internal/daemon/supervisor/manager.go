package supervisor

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net/netip"
	"os"
	"path/filepath"
	"strings"

	"ployz/internal/check"

	"ployz/internal/adapter/corrosion"
	"ployz/internal/adapter/platform"
	"ployz/internal/adapter/sqlite"
	"ployz/internal/engine"
	netctrl "ployz/internal/mesh"
	"ployz/internal/reconcile"
	"ployz/pkg/sdk/client"
	"ployz/pkg/sdk/defaults"
	"ployz/pkg/sdk/types"
)

// Compile-time check: Manager implements client.API.
var _ client.API = (*Manager)(nil)

type Manager struct {
	ctx        context.Context
	dataRoot   string
	store      SpecStore
	stateStore netctrl.StateStore
	ctrl       *netctrl.Controller
	engine     *engine.Engine
}

type managerCfg struct {
	specStore  SpecStore
	stateStore netctrl.StateStore
	ctrl       *netctrl.Controller
	eng        *engine.Engine
}

// ManagerOption configures a Manager.
type ManagerOption func(*managerCfg)

// WithSpecStore injects a SpecStore (default: sqlite).
func WithSpecStore(s SpecStore) ManagerOption {
	return func(c *managerCfg) { c.specStore = s }
}

// WithManagerStateStore injects a mesh.StateStore (default: sqlite).
func WithManagerStateStore(s netctrl.StateStore) ManagerOption {
	return func(c *managerCfg) { c.stateStore = s }
}

// WithManagerController injects a pre-built Controller.
func WithManagerController(ctrl *netctrl.Controller) ManagerOption {
	return func(c *managerCfg) { c.ctrl = ctrl }
}

// WithManagerEngine injects a pre-built Engine.
func WithManagerEngine(e *engine.Engine) ManagerOption {
	return func(c *managerCfg) { c.eng = e }
}

func New(ctx context.Context, dataRoot string, opts ...ManagerOption) (*Manager, error) {
	log := slog.With("component", "supervisor")
	if strings.TrimSpace(dataRoot) == "" {
		dataRoot = defaults.DataRoot()
	}

	var cfg managerCfg
	for _, o := range opts {
		o(&cfg)
	}

	// When all deps are injected, skip platform setup entirely.
	if cfg.specStore == nil || cfg.stateStore == nil || cfg.ctrl == nil || cfg.eng == nil {
		if err := initPlatformDefaults(ctx, dataRoot, &cfg); err != nil {
			return nil, err
		}
	}

	check.Assert(cfg.specStore != nil, "specStore must be set after init")
	check.Assert(cfg.stateStore != nil, "stateStore must be set after init")
	check.Assert(cfg.ctrl != nil, "ctrl must be set after init")
	check.Assert(cfg.eng != nil, "eng must be set after init")

	m := &Manager{
		ctx:        ctx,
		dataRoot:   dataRoot,
		store:      cfg.specStore,
		stateStore: cfg.stateStore,
		ctrl:       cfg.ctrl,
		engine:     cfg.eng,
	}

	// Start workers for all enabled specs.
	if specs, err := m.store.ListSpecs(); err == nil {
		for _, item := range specs {
			if !item.Enabled {
				continue
			}
			network := defaults.NormalizeNetwork(item.Spec.Network)
			if network == "" {
				continue
			}
			item.Spec.Network = network
			if item.Spec.DataRoot == "" {
				item.Spec.DataRoot = dataRoot
			}
			log.Info("restoring enabled network", "network", network)
			if startErr := m.engine.StartNetwork(ctx, item.Spec); startErr != nil {
				log.Warn("failed to restore network worker", "network", network, "err", startErr)
			}
		}
	}

	go func() {
		<-ctx.Done()
		log.Info("stopping")
		m.engine.StopAll()
		_ = m.ctrl.Close()  // best-effort cleanup
		_ = m.store.Close() // best-effort cleanup
	}()

	return m, nil
}

// initPlatformDefaults fills any nil fields on cfg with real platform
// implementations backed by SQLite, Corrosion, and the platform controller.
func initPlatformDefaults(ctx context.Context, dataRoot string, cfg *managerCfg) error {
	log := slog.With("component", "supervisor")
	log.Debug("initializing", "data_root", dataRoot)

	if err := defaults.EnsureDataRoot(dataRoot); err != nil {
		return err
	}

	statePath := filepath.Join(dataRoot, "daemon.db")
	sqlStore, err := sqlite.Open(statePath)
	if err != nil {
		return err
	}
	cfg.specStore = &sqliteSpecStore{s: sqlStore}

	registryFactory := netctrl.RegistryFactory(func(addr netip.AddrPort, token string) netctrl.Registry {
		return corrosion.NewStore(addr, token)
	})
	netStateStore := sqlite.NetworkStateStore{}
	cfg.stateStore = netStateStore

	ctrl, err := platform.NewController(netctrl.WithRegistryFactory(registryFactory))
	if err != nil {
		_ = cfg.specStore.Close() // best-effort cleanup
		return err
	}
	cfg.ctrl = ctrl

	cfg.eng = engine.New(ctx,
		engine.WithControllerFactory(func() (engine.NetworkController, error) {
			return platform.NewController(netctrl.WithRegistryFactory(registryFactory))
		}),
		engine.WithPeerReconcilerFactory(func() (reconcile.PeerReconciler, error) {
			return platform.NewController(netctrl.WithRegistryFactory(registryFactory))
		}),
		engine.WithRegistryFactory(func(addr netip.AddrPort, token string) reconcile.Registry {
			return corrosion.NewStore(addr, token)
		}),
		engine.WithStateStore(netStateStore),
	)

	if err := startPlatformServices(ctx); err != nil {
		_ = ctrl.Close()          // best-effort cleanup
		_ = cfg.specStore.Close() // best-effort cleanup
		return err
	}

	return nil
}

func (m *Manager) ApplyNetworkSpec(ctx context.Context, spec types.NetworkSpec) (types.ApplyResult, error) {
	m.normalizeSpec(&spec)
	if spec.Network == "" {
		return types.ApplyResult{}, fmt.Errorf("network is required")
	}
	log := slog.With("component", "supervisor", "network", spec.Network)
	log.Info("apply network spec requested")

	result, err := m.applyOnce(ctx, spec)
	if err != nil {
		log.Error("apply network spec failed", "err", err)
		return types.ApplyResult{}, err
	}
	if err := m.store.SaveSpec(spec, true); err != nil {
		return types.ApplyResult{}, err
	}

	// Start the convergence worker in-process.
	if err := m.engine.StartNetwork(m.ctx, spec); err != nil {
		return types.ApplyResult{}, fmt.Errorf("start convergence worker: %w", err)
	}

	running, _ := m.engine.Status(spec.Network)
	result.ConvergenceRunning = running
	log.Info("network apply complete", "worker_running", running)

	return result, nil
}

func (m *Manager) DisableNetwork(ctx context.Context, network string, purge bool) error {
	network = defaults.NormalizeNetwork(network)
	if network == "" {
		return fmt.Errorf("network is required")
	}
	log := slog.With("component", "supervisor", "network", network, "purge", purge)
	log.Info("disable requested")

	spec, cfg, err := m.resolveConfig(network)
	if err != nil {
		return err
	}

	// Stop the convergence worker first.
	if stopErr := m.engine.StopNetwork(network); stopErr != nil {
		log.Warn("failed to stop convergence worker", "err", stopErr)
	}

	if _, err := m.ctrl.Stop(ctx, cfg, purge); err != nil {
		return err
	}

	if purge {
		if err := m.store.DeleteSpec(network); err != nil {
			log.Error("delete persisted spec failed", "err", err)
			return err
		}
	} else {
		if err := m.store.SaveSpec(spec, false); err != nil {
			log.Error("persist disabled spec failed", "err", err)
			return err
		}
	}

	log.Info("disable complete")

	return nil
}

func (m *Manager) GetStatus(ctx context.Context, network string) (types.NetworkStatus, error) {
	network = defaults.NormalizeNetwork(network)
	_, cfg, err := m.resolveConfig(network)
	if err != nil {
		return types.NetworkStatus{}, err
	}

	status, err := m.ctrl.Status(ctx, cfg)
	if err != nil {
		return types.NetworkStatus{}, err
	}

	running, _ := m.engine.Status(network)
	health := m.engine.Health(network)

	return types.NetworkStatus{
		Configured:    status.Configured,
		Running:       status.Running,
		WireGuard:     status.WireGuard,
		Corrosion:     status.Corrosion,
		DockerNet:     status.DockerNet,
		StatePath:     status.StatePath,
		WorkerRunning: running,
		ClockHealth:   clockHealth(health.NTPStatus),
	}, nil
}

func (m *Manager) GetIdentity(_ context.Context, network string) (types.Identity, error) {
	spec, cfg, err := m.resolveConfig(defaults.NormalizeNetwork(network))
	if err != nil {
		return types.Identity{}, err
	}
	st, err := netctrl.LoadState(m.stateStore, cfg)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return types.Identity{}, fmt.Errorf("network %q: %w", spec.Network, netctrl.ErrNotInitialized)
		}
		return types.Identity{}, err
	}

	return types.Identity{
		ID:                st.WGPublic,
		PublicKey:         st.WGPublic,
		Subnet:            st.Subnet,
		ManagementIP:      st.Management,
		AdvertiseEndpoint: st.Advertise,
		NetworkCIDR:       st.CIDR,
		WGInterface:       st.WGInterface,
		WGPort:            st.WGPort,
		HelperName:        cfg.HelperName,
		CorrosionGossipPort: cfg.CorrosionGossipPort,
		CorrosionMemberID: st.CorrosionMemberID,
		CorrosionAPIToken: st.CorrosionAPIToken,
		Running:           st.Running,
	}, nil
}

func (m *Manager) ListMachines(ctx context.Context, network string) ([]types.MachineEntry, error) {
	network = defaults.NormalizeNetwork(network)
	_, cfg, err := m.resolveConfig(network)
	if err != nil {
		return nil, err
	}

	rows, err := m.ctrl.ListMachines(ctx, cfg)
	if err != nil {
		return nil, err
	}

	health := m.engine.Health(network)

	out := make([]types.MachineEntry, 0, len(rows))
	for _, row := range rows {
		entry := types.MachineEntry{
			ID:           row.ID,
			PublicKey:    row.PublicKey,
			Subnet:       row.Subnet,
			ManagementIP: row.ManagementIP,
			Endpoint:     row.Endpoint,
			LastUpdated:  row.LastUpdated,
			Version:      row.Version,
		}
		if ph, ok := health.Peers[row.ID]; ok {
			entry.Freshness = ph.Freshness
			entry.Stale = ph.Stale
			entry.ReplicationLag = ph.ReplicationLag
		}
		out = append(out, entry)
	}
	return out, nil
}

func (m *Manager) UpsertMachine(ctx context.Context, network string, entry types.MachineEntry) error {
	_, cfg, err := m.resolveConfig(defaults.NormalizeNetwork(network))
	if err != nil {
		return err
	}

	err = m.ctrl.UpsertMachine(ctx, cfg, netctrl.Machine{
		ID:              entry.ID,
		PublicKey:       entry.PublicKey,
		Subnet:          entry.Subnet,
		ManagementIP:    entry.ManagementIP,
		Endpoint:        entry.Endpoint,
		ExpectedVersion: entry.ExpectedVersion,
	})
	if errors.Is(err, netctrl.ErrConflict) {
		return fmt.Errorf("machine upsert conflict: %w", netctrl.ErrConflict)
	}
	if err != nil {
		return fmt.Errorf("upsert machine: %w", err)
	}
	return nil
}

func (m *Manager) RemoveMachine(ctx context.Context, network, idOrEndpoint string) error {
	_, cfg, err := m.resolveConfig(defaults.NormalizeNetwork(network))
	if err != nil {
		return err
	}

	return m.ctrl.RemoveMachine(ctx, cfg, idOrEndpoint)
}

func (m *Manager) TriggerReconcile(ctx context.Context, network string) error {
	network = defaults.NormalizeNetwork(network)
	log := slog.With("component", "supervisor", "network", network)
	log.Debug("trigger reconcile requested")

	// Stop and restart the worker — forces a fresh reconciliation.
	if stopErr := m.engine.StopNetwork(network); stopErr != nil {
		log.Warn("failed to stop worker before reconcile", "err", stopErr)
	}

	spec, cfg, err := m.resolveConfig(network)
	if err != nil {
		return err
	}

	_, err = m.ctrl.Reconcile(ctx, cfg)
	if err != nil {
		log.Error("imperative reconcile failed", "err", err)
		return err
	}

	// Restart convergence worker.
	if startErr := m.engine.StartNetwork(m.ctx, spec); startErr != nil {
		log.Warn("failed to restart convergence worker", "err", startErr)
	}
	log.Debug("worker restart requested")

	return nil
}

func (m *Manager) GetPeerHealth(ctx context.Context, network string) ([]types.PeerHealthResponse, error) {
	network = defaults.NormalizeNetwork(network)
	health := m.engine.Health(network)

	// Determine self ID.
	selfID := ""
	if identity, err := m.GetIdentity(ctx, network); err == nil {
		selfID = identity.ID
	}

	peers := make([]types.PeerLag, 0, len(health.Peers))
	for nodeID, ph := range health.Peers {
		peers = append(peers, types.PeerLag{
			NodeID:         nodeID,
			Freshness:      ph.Freshness,
			Stale:          ph.Stale,
			ReplicationLag: ph.ReplicationLag,
			PingRTT:        ph.PingRTT,
		})
	}

	return []types.PeerHealthResponse{
		{
			NodeID: selfID,
			NTP:    clockHealth(health.NTPStatus),
			Peers:  peers,
		},
	}, nil
}

// MachineAddr holds minimal machine info for proxy routing.
type MachineAddr struct {
	ID           string
	ManagementIP string
	OverlayIP    string
}

// ListMachineAddrs returns machine info for proxy routing.
func (m *Manager) ListMachineAddrs(ctx context.Context, network string) ([]MachineAddr, error) {
	network = defaults.NormalizeNetwork(network)
	_, cfg, err := m.resolveConfig(network)
	if err != nil {
		return nil, err
	}

	rows, err := m.ctrl.ListMachines(ctx, cfg)
	if err != nil {
		return nil, err
	}

	out := make([]MachineAddr, 0, len(rows))
	for _, row := range rows {
		if row.ManagementIP == "" {
			continue
		}
		addr := MachineAddr{
			ID:           row.ID,
			ManagementIP: row.ManagementIP,
		}
		if prefix, err := netip.ParsePrefix(row.Subnet); err == nil {
			addr.OverlayIP = netctrl.MachineIP(prefix).String()
		}
		out = append(out, addr)
	}
	return out, nil
}

func (m *Manager) normalizeSpec(spec *types.NetworkSpec) {
	spec.Network = defaults.NormalizeNetwork(spec.Network)
	if spec.DataRoot == "" {
		spec.DataRoot = m.dataRoot
	}
}

func (m *Manager) resolveSpec(network string) (types.NetworkSpec, error) {
	if network == "" {
		return types.NetworkSpec{}, fmt.Errorf("network is required")
	}
	persisted, ok, err := m.store.GetSpec(network)
	if err != nil {
		return types.NetworkSpec{}, err
	}
	if ok {
		m.normalizeSpec(&persisted.Spec)
		return persisted.Spec, nil
	}
	spec := types.NetworkSpec{Network: network}
	m.normalizeSpec(&spec)
	return spec, nil
}

func (m *Manager) resolveConfig(network string) (types.NetworkSpec, netctrl.Config, error) {
	spec, err := m.resolveSpec(network)
	if err != nil {
		return types.NetworkSpec{}, netctrl.Config{}, err
	}
	cfg, err := netctrl.ConfigFromSpec(spec)
	if err != nil {
		return types.NetworkSpec{}, netctrl.Config{}, err
	}
	return spec, cfg, nil
}

func (m *Manager) applyOnce(ctx context.Context, spec types.NetworkSpec) (types.ApplyResult, error) {
	cfg, err := netctrl.ConfigFromSpec(spec)
	if err != nil {
		return types.ApplyResult{}, err
	}

	out, err := m.ctrl.Start(ctx, cfg)
	if err != nil {
		return types.ApplyResult{}, err
	}

	return types.ApplyResult{
		Network:           out.Network,
		NetworkCIDR:       out.NetworkCIDR.String(),
		Subnet:            out.Subnet.String(),
		ManagementIP:      out.Management.String(),
		WGInterface:       out.WGInterface,
		WGPort:            out.WGPort,
		AdvertiseEndpoint: out.AdvertiseEndpoint,
		CorrosionName:     out.CorrosionName,
		CorrosionAPIAddr:  out.CorrosionAPIAddr.String(),
		CorrosionGossipAddrPort: out.CorrosionGossipAddrPort.String(),
		DockerNetwork:     out.DockerNetwork,
	}, nil
}

func clockHealth(ntp reconcile.NTPStatus) types.ClockHealth {
	return types.ClockHealth{
		NTPOffsetMs: float64(ntp.Offset.Milliseconds()),
		NTPHealthy:  ntp.Healthy,
		NTPError:    ntp.Error,
	}
}

// sqliteSpecStore adapts *sqlite.Store to the SpecStore interface.
type sqliteSpecStore struct {
	s *sqlite.Store
}

func (a *sqliteSpecStore) SaveSpec(spec types.NetworkSpec, enabled bool) error {
	return a.s.SaveSpec(spec, enabled)
}

func (a *sqliteSpecStore) GetSpec(network string) (PersistedSpec, bool, error) {
	p, ok, err := a.s.GetSpec(network)
	if err != nil || !ok {
		return PersistedSpec{}, ok, err
	}
	return PersistedSpec{Spec: p.Spec, Enabled: p.Enabled}, true, nil
}

func (a *sqliteSpecStore) ListSpecs() ([]PersistedSpec, error) {
	items, err := a.s.ListSpecs()
	if err != nil {
		return nil, err
	}
	out := make([]PersistedSpec, len(items))
	for i, item := range items {
		out[i] = PersistedSpec{Spec: item.Spec, Enabled: item.Enabled}
	}
	return out, nil
}

func (a *sqliteSpecStore) DeleteSpec(network string) error {
	return a.s.DeleteSpec(network)
}

func (a *sqliteSpecStore) Close() error {
	return a.s.Close()
}
