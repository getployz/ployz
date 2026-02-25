package manager

import (
	"fmt"

	"ployz/internal/network"
	"ployz/pkg/sdk/defaults"
	"ployz/pkg/sdk/types"
)

func (m *Manager) normalizeSpec(spec *types.NetworkSpec) {
	spec.Network = defaults.NormalizeNetwork(spec.Network)
	if spec.DataRoot == "" {
		spec.DataRoot = m.dataRoot
	}
}

func (m *Manager) resolveSpec() (types.NetworkSpec, error) {
	persisted, ok, err := m.store.GetSpec()
	if err != nil {
		return types.NetworkSpec{}, err
	}
	if !ok {
		return types.NetworkSpec{}, fmt.Errorf("%w", ErrNetworkNotConfigured)
	}
	m.normalizeSpec(&persisted.Spec)
	if persisted.Spec.Network == "" {
		return types.NetworkSpec{}, fmt.Errorf("network is required")
	}
	return persisted.Spec, nil
}

func (m *Manager) resolveConfig() (types.NetworkSpec, network.Config, error) {
	spec, err := m.resolveSpec()
	if err != nil {
		return types.NetworkSpec{}, network.Config{}, err
	}
	cfg, err := network.ConfigFromSpec(spec)
	if err != nil {
		return types.NetworkSpec{}, network.Config{}, err
	}
	return spec, cfg, nil
}
