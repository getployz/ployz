package api

import (
	"context"

	"ployz/internal/controlplane/manager"
	proxymod "ployz/internal/controlplane/proxy"
)

// proxyMapper adapts the controlplane manager to the proxy.MachineMapper interface.
type proxyMapper struct {
	manager *manager.Manager
}

func (m proxyMapper) ListMachines(ctx context.Context) ([]proxymod.MachineInfo, error) {
	addrs, err := m.manager.ListMachineAddrs(ctx)
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
