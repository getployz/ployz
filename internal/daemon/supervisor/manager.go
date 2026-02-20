package supervisor

import (
	"context"
	"errors"
	"fmt"
	"net/netip"
	"os"
	"path/filepath"
	"strings"
	"sync"

	pb "ployz/internal/daemon/pb"
	"ployz/internal/daemon/reconcile"
	"ployz/internal/machine"
	"ployz/internal/machine/registry"
)

type workerHandle struct {
	cancel context.CancelFunc
	done   chan struct{}
	spec   *pb.NetworkSpec
}

type Manager struct {
	ctx      context.Context
	dataRoot string
	store    *specStore
	ctrl     *machine.Controller
	hub      *eventHub

	mu      sync.Mutex
	workers map[string]*workerHandle
}

func New(ctx context.Context, dataRoot string) (*Manager, error) {
	if strings.TrimSpace(dataRoot) == "" {
		dataRoot = machine.DefaultDataRoot()
	}
	statePath := filepath.Join(dataRoot, "daemon.db")
	store, err := newSpecStore(statePath)
	if err != nil {
		return nil, err
	}
	ctrl, err := machine.New()
	if err != nil {
		_ = store.close()
		return nil, err
	}

	m := &Manager{
		ctx:      ctx,
		dataRoot: dataRoot,
		store:    store,
		ctrl:     ctrl,
		hub:      newEventHub(),
		workers:  make(map[string]*workerHandle),
	}

	persisted, err := m.store.list()
	if err != nil {
		_ = m.ctrl.Close()
		_ = m.store.close()
		return nil, err
	}

	m.mu.Lock()
	for _, item := range persisted {
		if !item.Enabled {
			continue
		}
		m.startWorkerLocked(item.Spec, false)
	}
	m.mu.Unlock()

	go func() {
		<-ctx.Done()
		m.stopAllWorkers()
		_ = m.ctrl.Close()
		_ = m.store.close()
	}()

	return m, nil
}

func (m *Manager) ApplyNetworkSpec(ctx context.Context, spec *pb.NetworkSpec) (*pb.ApplyResult, error) {
	m.normalizeSpec(spec)
	if spec.Network == "" {
		return nil, fmt.Errorf("network is required")
	}

	if err := m.store.save(spec, true); err != nil {
		return nil, err
	}

	result, err := m.applyOnce(ctx, spec)
	if err != nil {
		return nil, err
	}

	m.mu.Lock()
	m.startWorkerLocked(spec, true)
	m.mu.Unlock()

	m.hub.publish(spec.Network, "apply.success", "applied network spec")
	result.ConvergenceRunning = true
	return result, nil
}

func (m *Manager) DisableNetwork(ctx context.Context, network string, purge bool) error {
	network = normalizeNetwork(network)
	if network == "" {
		return fmt.Errorf("network is required")
	}

	m.mu.Lock()
	m.stopWorkerLocked(network)
	m.mu.Unlock()

	spec, err := m.resolveSpec(network)
	if err != nil {
		return err
	}

	cfg, err := configFromSpec(spec)
	if err != nil {
		return err
	}

	if _, err := m.ctrl.Stop(ctx, cfg, purge); err != nil {
		return err
	}

	if purge {
		if err := m.store.delete(network); err != nil {
			return err
		}
	} else {
		if err := m.store.save(spec, false); err != nil {
			return err
		}
	}

	m.hub.publish(network, "disable.success", "disabled network")
	return nil
}

func (m *Manager) GetStatus(ctx context.Context, network string) (*pb.NetworkStatus, error) {
	spec, err := m.resolveSpec(normalizeNetwork(network))
	if err != nil {
		return nil, err
	}
	cfg, err := configFromSpec(spec)
	if err != nil {
		return nil, err
	}

	status, err := m.ctrl.Status(ctx, cfg)
	if err != nil {
		return nil, err
	}

	m.mu.Lock()
	_, running := m.workers[spec.Network]
	m.mu.Unlock()

	return &pb.NetworkStatus{
		Configured:    status.Configured,
		Running:       status.Running,
		Wireguard:     status.WireGuard,
		Corrosion:     status.Corrosion,
		Docker:        status.DockerNet,
		StatePath:     status.StatePath,
		WorkerRunning: running,
	}, nil
}

func (m *Manager) GetIdentity(_ context.Context, network string) (*pb.Identity, error) {
	spec, err := m.resolveSpec(normalizeNetwork(network))
	if err != nil {
		return nil, err
	}
	cfg, err := configFromSpec(spec)
	if err != nil {
		return nil, err
	}
	st, err := machine.LoadState(cfg)
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
		Running:             st.Running,
	}, nil
}

func (m *Manager) ListMachines(ctx context.Context, network string) ([]*pb.MachineEntry, error) {
	spec, err := m.resolveSpec(normalizeNetwork(network))
	if err != nil {
		return nil, err
	}
	cfg, err := configFromSpec(spec)
	if err != nil {
		return nil, err
	}

	rows, err := m.ctrl.ListMachines(ctx, cfg)
	if err != nil {
		return nil, err
	}
	out := make([]*pb.MachineEntry, 0, len(rows))
	for _, row := range rows {
		out = append(out, &pb.MachineEntry{
			Id:           row.ID,
			PublicKey:    row.PublicKey,
			Subnet:       row.Subnet,
			ManagementIp: row.Management,
			Endpoint:     row.Endpoint,
			LastUpdated:  row.LastUpdated,
			Version:      row.Version,
		})
	}
	return out, nil
}

func (m *Manager) UpsertMachine(ctx context.Context, network string, entry *pb.MachineEntry) error {
	spec, err := m.resolveSpec(normalizeNetwork(network))
	if err != nil {
		return err
	}
	cfg, err := configFromSpec(spec)
	if err != nil {
		return err
	}

	err = m.ctrl.UpsertMachine(ctx, cfg, machine.Machine{
		ID:              entry.Id,
		PublicKey:       entry.PublicKey,
		Subnet:          entry.Subnet,
		Management:      entry.ManagementIp,
		Endpoint:        entry.Endpoint,
		ExpectedVersion: entry.ExpectedVersion,
	})
	if errors.Is(err, registry.ErrConflict) {
		return fmt.Errorf("machine upsert conflict: %w", registry.ErrConflict)
	}
	if err != nil {
		return fmt.Errorf("upsert machine: %w", err)
	}
	return nil
}

func (m *Manager) RemoveMachine(ctx context.Context, network, idOrEndpoint string) error {
	spec, err := m.resolveSpec(normalizeNetwork(network))
	if err != nil {
		return err
	}
	cfg, err := configFromSpec(spec)
	if err != nil {
		return err
	}

	return m.ctrl.RemoveMachine(ctx, cfg, idOrEndpoint)
}

func (m *Manager) TriggerReconcile(ctx context.Context, network string) error {
	spec, err := m.resolveSpec(normalizeNetwork(network))
	if err != nil {
		return err
	}
	cfg, err := configFromSpec(spec)
	if err != nil {
		return err
	}

	count, err := m.ctrl.Reconcile(ctx, cfg)
	if err != nil {
		m.hub.publish(spec.Network, "reconcile.error", err.Error())
		return err
	}
	m.hub.publish(spec.Network, "reconcile.success", fmt.Sprintf("reconciled %d peers", count))
	return nil
}

func (m *Manager) StreamEvents(ctx context.Context, network string) (<-chan *pb.Event, error) {
	network = normalizeNetwork(network)
	if network == "" {
		return nil, fmt.Errorf("network is required")
	}
	return m.hub.subscribe(ctx, network), nil
}

func (m *Manager) startWorkerLocked(spec *pb.NetworkSpec, alreadyApplied bool) {
	network := normalizeNetwork(spec.Network)
	if network == "" {
		return
	}
	spec.Network = network
	if existing, ok := m.workers[network]; ok {
		existing.cancel()
		<-existing.done
	}

	ctx, cancel := context.WithCancel(m.ctx)
	h := &workerHandle{cancel: cancel, done: make(chan struct{}), spec: spec}
	m.workers[network] = h

	go func() {
		defer close(h.done)
		if !alreadyApplied {
			result, err := m.applyOnce(ctx, spec)
			if err != nil {
				m.hub.publish(network, "apply.error", err.Error())
			} else {
				m.hub.publish(network, "apply.success", fmt.Sprintf("ready on %s", result.WgInterface))
			}
		}

		cfg, err := configFromSpec(spec)
		if err != nil {
			m.hub.publish(network, "worker.error", err.Error())
			return
		}
		w := reconcile.Worker{
			Spec: cfg,
			OnEvent: func(eventType, message string) {
				m.hub.publish(network, eventType, message)
			},
			OnFailure: func(err error) {
				m.hub.publish(network, "worker.error", err.Error())
			},
		}
		if err := w.Run(ctx); err != nil && !errors.Is(err, context.Canceled) {
			m.hub.publish(network, "worker.error", err.Error())
		}
	}()
}

func (m *Manager) stopWorkerLocked(network string) {
	if existing, ok := m.workers[network]; ok {
		existing.cancel()
		<-existing.done
		delete(m.workers, network)
	}
}

func (m *Manager) stopAllWorkers() {
	m.mu.Lock()
	networks := make([]string, 0, len(m.workers))
	for network := range m.workers {
		networks = append(networks, network)
	}
	m.mu.Unlock()

	for _, network := range networks {
		m.mu.Lock()
		m.stopWorkerLocked(network)
		m.mu.Unlock()
	}
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

func (m *Manager) normalizeSpec(spec *pb.NetworkSpec) {
	spec.Network = normalizeNetwork(spec.Network)
	if spec.DataRoot == "" {
		spec.DataRoot = m.dataRoot
	}
}

func (m *Manager) resolveSpec(network string) (*pb.NetworkSpec, error) {
	if network == "" {
		return nil, fmt.Errorf("network is required")
	}
	persisted, ok, err := m.store.get(network)
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

func normalizeNetwork(network string) string {
	network = strings.TrimSpace(network)
	if network == "" {
		return "default"
	}
	return network
}

func configFromSpec(spec *pb.NetworkSpec) (machine.Config, error) {
	cfg := machine.Config{
		Network:     normalizeNetwork(spec.Network),
		DataRoot:    strings.TrimSpace(spec.DataRoot),
		AdvertiseEP: strings.TrimSpace(spec.AdvertiseEndpoint),
		WGPort:      int(spec.WgPort),
		HelperImage: strings.TrimSpace(spec.HelperImage),
	}
	for _, bs := range spec.Bootstrap {
		bs = strings.TrimSpace(bs)
		if bs == "" {
			continue
		}
		cfg.CorrosionBootstrap = append(cfg.CorrosionBootstrap, bs)
	}

	if strings.TrimSpace(spec.NetworkCidr) != "" {
		pfx, err := netip.ParsePrefix(strings.TrimSpace(spec.NetworkCidr))
		if err != nil {
			return machine.Config{}, fmt.Errorf("parse network cidr: %w", err)
		}
		cfg.NetworkCIDR = pfx
	}
	if strings.TrimSpace(spec.Subnet) != "" {
		pfx, err := netip.ParsePrefix(strings.TrimSpace(spec.Subnet))
		if err != nil {
			return machine.Config{}, fmt.Errorf("parse subnet: %w", err)
		}
		cfg.Subnet = pfx
	}
	if strings.TrimSpace(spec.ManagementIp) != "" {
		addr, err := netip.ParseAddr(strings.TrimSpace(spec.ManagementIp))
		if err != nil {
			return machine.Config{}, fmt.Errorf("parse management ip: %w", err)
		}
		cfg.Management = addr
	}

	return machine.NormalizeConfig(cfg)
}
