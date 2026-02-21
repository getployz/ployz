package supervisor

import (
	"context"
	"errors"
	"fmt"
	"net/netip"
	"os"
	"path/filepath"
	"strings"

	"ployz/internal/control/state"
	"ployz/internal/coordination/registry"
	pb "ployz/internal/daemon/pb"
	netctrl "ployz/internal/machine/network"
	"ployz/pkg/sdk/defaults"
)

type Manager struct {
	ctx      context.Context
	dataRoot string
	store    *state.Store
	ctrl     *netctrl.Controller
}

func New(ctx context.Context, dataRoot string) (*Manager, error) {
	if strings.TrimSpace(dataRoot) == "" {
		dataRoot = defaults.DataRoot()
	}
	statePath := filepath.Join(dataRoot, "daemon.db")
	store, err := state.Open(statePath)
	if err != nil {
		return nil, err
	}
	ctrl, err := netctrl.New()
	if err != nil {
		_ = store.Close()
		return nil, err
	}

	m := &Manager{
		ctx:      ctx,
		dataRoot: dataRoot,
		store:    store,
		ctrl:     ctrl,
	}

	go func() {
		<-ctx.Done()
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

	result, err := m.applyOnce(ctx, spec)
	if err != nil {
		return nil, err
	}
	if err := m.store.SaveSpec(spec, true); err != nil {
		return nil, err
	}

	rt, ok, err := m.store.GetRuntimeStatus(spec.Network)
	if err == nil && ok {
		result.ConvergenceRunning = rt.Running
	} else {
		result.ConvergenceRunning = true
	}

	return result, nil
}

func (m *Manager) DisableNetwork(ctx context.Context, network string, purge bool) error {
	network = defaults.NormalizeNetwork(network)
	if network == "" {
		return fmt.Errorf("network is required")
	}

	spec, cfg, err := m.resolveConfig(network)
	if err != nil {
		return err
	}

	if _, err := m.ctrl.Stop(ctx, cfg, purge); err != nil {
		return err
	}

	if purge {
		if err := m.store.DeleteSpec(network); err != nil {
			return err
		}
	} else {
		if err := m.store.SaveSpec(spec, false); err != nil {
			return err
		}
	}

	_ = m.store.SetRuntimeStatus(network, false, "")
	return nil
}

func (m *Manager) GetStatus(ctx context.Context, network string) (*pb.NetworkStatus, error) {
	spec, cfg, err := m.resolveConfig(defaults.NormalizeNetwork(network))
	if err != nil {
		return nil, err
	}

	status, err := m.ctrl.Status(ctx, cfg)
	if err != nil {
		return nil, err
	}

	running := false
	runtimeStatus, ok, err := m.store.GetRuntimeStatus(spec.Network)
	if err == nil && ok {
		running = runtimeStatus.Running
	}

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
	spec, cfg, err := m.resolveConfig(defaults.NormalizeNetwork(network))
	if err != nil {
		return nil, err
	}
	st, err := netctrl.LoadState(cfg)
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
	_, cfg, err := m.resolveConfig(defaults.NormalizeNetwork(network))
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
	if errors.Is(err, registry.ErrConflict) {
		return fmt.Errorf("machine upsert conflict: %w", registry.ErrConflict)
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
	spec, cfg, err := m.resolveConfig(defaults.NormalizeNetwork(network))
	if err != nil {
		return err
	}

	_, err = m.ctrl.Reconcile(ctx, cfg)
	m.recordReconcileResult(spec.Network, err)
	return err
}

func (m *Manager) recordReconcileResult(network string, reconcileErr error) {
	runtimeStatus, ok, err := m.store.GetRuntimeStatus(network)
	if err != nil {
		return
	}
	running := ok && runtimeStatus.Running
	if reconcileErr != nil {
		_ = m.store.SetRuntimeStatus(network, running, reconcileErr.Error())
		return
	}
	_ = m.store.SetRuntimeStatus(network, running, "")
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
		bs = strings.TrimSpace(bs)
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
