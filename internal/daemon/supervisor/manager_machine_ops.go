package supervisor

import (
	"context"
	"errors"
	"fmt"
	"net/netip"
	"os"

	netctrl "ployz/internal/mesh"
	"ployz/pkg/sdk/defaults"
	"ployz/pkg/sdk/types"
)

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
		ID:                  st.WGPublic,
		PublicKey:           st.WGPublic,
		Subnet:              st.Subnet,
		ManagementIP:        st.Management,
		AdvertiseEndpoint:   st.Advertise,
		NetworkCIDR:         st.CIDR,
		WGInterface:         st.WGInterface,
		WGPort:              st.WGPort,
		HelperName:          cfg.HelperName,
		CorrosionGossipPort: cfg.CorrosionGossipPort,
		CorrosionMemberID:   st.CorrosionMemberID,
		CorrosionAPIToken:   st.CorrosionAPIToken,
		Running:             st.Running,
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
