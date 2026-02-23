package supervisor

import (
	"fmt"

	netctrl "ployz/internal/mesh"
	"ployz/pkg/sdk/defaults"
	"ployz/pkg/sdk/types"
)

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
