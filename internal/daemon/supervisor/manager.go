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

	"ployz/internal/adapter/corrosion"
	"ployz/internal/adapter/platform"
	"ployz/internal/adapter/sqlite"
	pb "ployz/internal/daemon/pb"
	"ployz/internal/engine"
	netctrl "ployz/internal/network"
	"ployz/internal/reconcile"
	"ployz/pkg/sdk/defaults"
)

type Manager struct {
	ctx        context.Context
	dataRoot   string
	store      *sqlite.Store
	stateStore netctrl.StateStore
	ctrl       *netctrl.Controller
	engine     *engine.Engine
}

func New(ctx context.Context, dataRoot string) (*Manager, error) {
	log := slog.With("component", "supervisor")
	if strings.TrimSpace(dataRoot) == "" {
		dataRoot = defaults.DataRoot()
	}
	log.Debug("initializing", "data_root", dataRoot)
	if err := defaults.EnsureDataRoot(dataRoot); err != nil {
		return nil, err
	}
	statePath := filepath.Join(dataRoot, "daemon.db")
	store, err := sqlite.Open(statePath)
	if err != nil {
		return nil, err
	}
	registryFactory := netctrl.RegistryFactory(func(addr netip.AddrPort, token string) netctrl.Registry {
		return corrosion.NewStore(addr, token)
	})
	netStateStore := sqlite.NetworkStateStore{}
	ctrl, err := platform.NewController(netctrl.WithRegistryFactory(registryFactory))
	if err != nil {
		_ = store.Close()
		return nil, err
	}

	eng := engine.New(ctx,
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

	m := &Manager{
		ctx:        ctx,
		dataRoot:   dataRoot,
		store:      store,
		stateStore: netStateStore,
		ctrl:       ctrl,
		engine:     eng,
	}
	if err := startPlatformServices(ctx); err != nil {
		_ = m.ctrl.Close()
		_ = m.store.Close()
		return nil, err
	}

	// Start workers for all enabled specs.
	if specs, err := store.ListSpecs(); err == nil {
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
			_ = eng.StartNetwork(ctx, item.Spec)
		}
	}

	go func() {
		<-ctx.Done()
		log.Info("stopping")
		eng.StopAll()
		_ = m.ctrl.Close()
		_ = m.store.Close()
	}()

	return m, nil
}

func (m *Manager) ApplyNetworkSpec(ctx context.Context, spec *pb.NetworkSpec) (*pb.ApplyResult, error) {
	m.normalizeSpec(spec)
	if spec.Network == "" {
		return nil, fmt.Errorf("network is required")
	}
	log := slog.With("component", "supervisor", "network", spec.Network)
	log.Info("apply network spec requested")

	result, err := m.applyOnce(ctx, spec)
	if err != nil {
		log.Error("apply network spec failed", "err", err)
		return nil, err
	}
	if err := m.store.SaveSpec(spec, true); err != nil {
		return nil, err
	}

	// Start the convergence worker in-process.
	_ = m.engine.StartNetwork(ctx, spec)

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
	_ = m.engine.StopNetwork(network)

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

func (m *Manager) GetStatus(ctx context.Context, network string) (*pb.NetworkStatus, error) {
	network = defaults.NormalizeNetwork(network)
	_, cfg, err := m.resolveConfig(network)
	if err != nil {
		return nil, err
	}

	status, err := m.ctrl.Status(ctx, cfg)
	if err != nil {
		return nil, err
	}

	running, _ := m.engine.Status(network)
	health := m.engine.Health(network)

	clockHealth := &pb.ClockHealth{
		NtpOffsetMs: float64(health.NTPStatus.Offset.Milliseconds()),
		NtpHealthy:  health.NTPStatus.Healthy,
		NtpError:    health.NTPStatus.Error,
	}

	return &pb.NetworkStatus{
		Configured:    status.Configured,
		Running:       status.Running,
		Wireguard:     status.WireGuard,
		Corrosion:     status.Corrosion,
		Docker:        status.DockerNet,
		StatePath:     status.StatePath,
		WorkerRunning: running,
		ClockHealth:   clockHealth,
	}, nil
}

func (m *Manager) GetIdentity(_ context.Context, network string) (*pb.Identity, error) {
	spec, cfg, err := m.resolveConfig(defaults.NormalizeNetwork(network))
	if err != nil {
		return nil, err
	}
	st, err := netctrl.LoadState(m.stateStore, cfg)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, fmt.Errorf("network %q is not initialized", spec.Network)
		}
		return nil, err
	}

	return &pb.Identity{
		Id:                  st.WGPublic,
		PublicKey:           st.WGPublic,
		Subnet:              st.Subnet,
		ManagementIp:        st.Management,
		AdvertiseEndpoint:   st.Advertise,
		NetworkCidr:         st.CIDR,
		WgInterface:         st.WGInterface,
		WgPort:              int32(st.WGPort),
		HelperName:          cfg.HelperName,
		CorrosionGossipPort: int32(cfg.CorrosionGossip),
		CorrosionMemberId:   st.CorrosionMemberID,
		CorrosionApiToken:   st.CorrosionAPIToken,
		Running:             st.Running,
	}, nil
}

func (m *Manager) ListMachines(ctx context.Context, network string) ([]*pb.MachineEntry, error) {
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

	out := make([]*pb.MachineEntry, 0, len(rows))
	for _, row := range rows {
		entry := &pb.MachineEntry{
			Id:           row.ID,
			PublicKey:    row.PublicKey,
			Subnet:       row.Subnet,
			ManagementIp: row.Management,
			Endpoint:     row.Endpoint,
			LastUpdated:  row.LastUpdated,
			Version:      row.Version,
		}
		if ph, ok := health.Peers[row.ID]; ok {
			entry.FreshnessMs = float64(ph.Freshness.Milliseconds())
			entry.Stale = ph.Stale
			entry.ReplicationLagMs = float64(ph.ReplicationLag.Milliseconds())
		}
		out = append(out, entry)
	}
	return out, nil
}

func (m *Manager) UpsertMachine(ctx context.Context, network string, entry *pb.MachineEntry) error {
	_, cfg, err := m.resolveConfig(defaults.NormalizeNetwork(network))
	if err != nil {
		return err
	}

	err = m.ctrl.UpsertMachine(ctx, cfg, netctrl.Machine{
		ID:              entry.Id,
		PublicKey:       entry.PublicKey,
		Subnet:          entry.Subnet,
		Management:      entry.ManagementIp,
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
	_ = m.engine.StopNetwork(network)

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
	_ = m.engine.StartNetwork(ctx, &pb.NetworkSpec{
		Network:           spec.Network,
		DataRoot:          spec.DataRoot,
		NetworkCidr:       spec.NetworkCidr,
		Subnet:            spec.Subnet,
		ManagementIp:      spec.ManagementIp,
		AdvertiseEndpoint: spec.AdvertiseEndpoint,
		WgPort:            spec.WgPort,
		Bootstrap:         spec.Bootstrap,
		HelperImage:       spec.HelperImage,
		CorrosionMemberId: spec.CorrosionMemberId,
		CorrosionApiToken: spec.CorrosionApiToken,
	})
	log.Debug("worker restart requested")

	return nil
}

func (m *Manager) GetPeerHealth(ctx context.Context, network string) (*pb.GetPeerHealthResponse, error) {
	network = defaults.NormalizeNetwork(network)
	health := m.engine.Health(network)

	// Determine self ID.
	selfID := ""
	if identity, err := m.GetIdentity(ctx, network); err == nil {
		selfID = identity.Id
	}

	peers := make([]*pb.PeerLag, 0, len(health.Peers))
	for nodeID, ph := range health.Peers {
		pingMs := float64(ph.PingRTT.Milliseconds())
		if ph.PingRTT < 0 {
			pingMs = -1
		} else if ph.PingRTT > 0 {
			// Sub-millisecond precision.
			pingMs = float64(ph.PingRTT.Microseconds()) / 1000.0
		}
		peers = append(peers, &pb.PeerLag{
			NodeId:           nodeID,
			FreshnessMs:      float64(ph.Freshness.Milliseconds()),
			Stale:            ph.Stale,
			ReplicationLagMs: float64(ph.ReplicationLag.Milliseconds()),
			PingMs:           pingMs,
		})
	}

	return &pb.GetPeerHealthResponse{
		Messages: []*pb.PeerHealthReply{
			{
				NodeId: selfID,
				Ntp: &pb.ClockHealth{
					NtpOffsetMs: float64(health.NTPStatus.Offset.Milliseconds()),
					NtpHealthy:  health.NTPStatus.Healthy,
					NtpError:    health.NTPStatus.Error,
				},
				Peers: peers,
			},
		},
	}, nil
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
		if row.Management == "" {
			continue
		}
		addr := MachineAddr{
			ID:           row.ID,
			ManagementIP: row.Management,
		}
		// Derive the machine's overlay IPv4 from its subnet (first host IP).
		if prefix, err := netip.ParsePrefix(row.Subnet); err == nil {
			addr.OverlayIP = prefix.Masked().Addr().Next().String()
		}
		out = append(out, addr)
	}
	return out, nil
}

// MachineAddr holds minimal machine info for proxy routing.
type MachineAddr struct {
	ID           string
	ManagementIP string
	OverlayIP    string
}

func (m *Manager) normalizeSpec(spec *pb.NetworkSpec) {
	spec.Network = defaults.NormalizeNetwork(spec.Network)
	if spec.DataRoot == "" {
		spec.DataRoot = m.dataRoot
	}
}

func (m *Manager) resolveSpec(network string) (*pb.NetworkSpec, error) {
	if network == "" {
		return nil, fmt.Errorf("network is required")
	}
	persisted, ok, err := m.store.GetSpec(network)
	if err != nil {
		return nil, err
	}
	if ok {
		m.normalizeSpec(persisted.Spec)
		return persisted.Spec, nil
	}
	spec := &pb.NetworkSpec{Network: network}
	m.normalizeSpec(spec)
	return spec, nil
}

func (m *Manager) resolveConfig(network string) (*pb.NetworkSpec, netctrl.Config, error) {
	spec, err := m.resolveSpec(network)
	if err != nil {
		return nil, netctrl.Config{}, err
	}
	cfg, err := configFromSpec(spec)
	if err != nil {
		return nil, netctrl.Config{}, err
	}
	return spec, cfg, nil
}

func (m *Manager) applyOnce(ctx context.Context, spec *pb.NetworkSpec) (*pb.ApplyResult, error) {
	cfg, err := configFromSpec(spec)
	if err != nil {
		return nil, err
	}

	out, err := m.ctrl.Start(ctx, cfg)
	if err != nil {
		return nil, err
	}

	return &pb.ApplyResult{
		Network:             out.Network,
		NetworkCidr:         out.NetworkCIDR.String(),
		Subnet:              out.Subnet.String(),
		ManagementIp:        out.Management.String(),
		WgInterface:         out.WGInterface,
		WgPort:              int32(out.WGPort),
		AdvertiseEndpoint:   out.AdvertiseEP,
		CorrosionName:       out.CorrosionName,
		CorrosionApiAddr:    out.CorrosionAPIAddr.String(),
		CorrosionGossipAddr: out.CorrosionGossipAP.String(),
		DockerNetwork:       out.DockerNetwork,
	}, nil
}

func configFromSpec(spec *pb.NetworkSpec) (netctrl.Config, error) {
	cfg := netctrl.Config{
		Network:           defaults.NormalizeNetwork(spec.Network),
		DataRoot:          strings.TrimSpace(spec.DataRoot),
		AdvertiseEP:       strings.TrimSpace(spec.AdvertiseEndpoint),
		WGPort:            int(spec.WgPort),
		CorrosionMemberID: spec.CorrosionMemberId,
		CorrosionAPIToken: strings.TrimSpace(spec.CorrosionApiToken),
		HelperImage:       strings.TrimSpace(spec.HelperImage),
	}
	for _, bs := range spec.Bootstrap {
		bs = netctrl.NormalizeBootstrapAddrPort(bs)
		if bs == "" {
			continue
		}
		cfg.CorrosionBootstrap = append(cfg.CorrosionBootstrap, bs)
	}

	if strings.TrimSpace(spec.NetworkCidr) != "" {
		pfx, err := netip.ParsePrefix(strings.TrimSpace(spec.NetworkCidr))
		if err != nil {
			return netctrl.Config{}, fmt.Errorf("parse network cidr: %w", err)
		}
		cfg.NetworkCIDR = pfx
	}
	if strings.TrimSpace(spec.Subnet) != "" {
		pfx, err := netip.ParsePrefix(strings.TrimSpace(spec.Subnet))
		if err != nil {
			return netctrl.Config{}, fmt.Errorf("parse subnet: %w", err)
		}
		cfg.Subnet = pfx
	}
	if strings.TrimSpace(spec.ManagementIp) != "" {
		addr, err := netip.ParseAddr(strings.TrimSpace(spec.ManagementIp))
		if err != nil {
			return netctrl.Config{}, fmt.Errorf("parse management ip: %w", err)
		}
		cfg.Management = addr
	}

	return netctrl.NormalizeConfig(cfg)
}
