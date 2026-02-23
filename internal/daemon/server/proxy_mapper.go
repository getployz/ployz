package server

import (
	"context"

	proxymod "ployz/internal/daemon/proxy"
	"ployz/internal/daemon/supervisor"
)

// proxyMapper adapts the supervisor manager to the proxy.MachineMapper interface.
type proxyMapper struct {
	manager *supervisor.Manager
}

func (m proxyMapper) ListMachines(ctx context.Context, network string) ([]proxymod.MachineInfo, error) {
	addrs, err := m.manager.ListMachineAddrs(ctx, network)
	if err != nil {
		return nil, err
	}
	out := make([]proxymod.MachineInfo, len(addrs))
	for i, a := range addrs {
		out[i] = proxymod.MachineInfo{
			ID:           a.ID,
			ManagementIP: a.ManagementIP,
			OverlayIP:    a.OverlayIP,
		}
	}
	return out, nil
}
