package machine

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"

	"ployz/machine/mesh"
)

const networkConfigFileName = "network.json"

// NetworkConfig is the persisted configuration for a machine's network membership.
// Written on init/join, read on subsequent boots.
type NetworkConfig struct {
	// Network is the name of the network this machine belongs to.
	Network string `json:"network"`
}

// HasNetworkConfig reports whether a saved network config exists on disk.
func (m *Machine) HasNetworkConfig() (bool, error) {
	_, err := os.Stat(m.networkConfigPath())
	if err == nil {
		return true, nil
	}
	if errors.Is(err, os.ErrNotExist) {
		return false, nil
	}
	return false, fmt.Errorf("check network config: %w", err)
}

// SaveNetworkConfig persists the network config to the data dir.
// Called during init/join.
func (m *Machine) SaveNetworkConfig(cfg NetworkConfig) error {
	data, err := json.MarshalIndent(cfg, "", "  ")
	if err != nil {
		return fmt.Errorf("marshal network config: %w", err)
	}

	path := m.networkConfigPath()
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return fmt.Errorf("create network config dir: %w", err)
	}

	if err := os.WriteFile(path, data, 0o600); err != nil {
		return fmt.Errorf("write network config: %w", err)
	}
	return nil
}

// RemoveNetworkConfig deletes the network config. Called when leaving a cluster.
// The network must already be stopped.
func (m *Machine) RemoveNetworkConfig() error {
	if m.Phase() != mesh.PhaseStopped {
		return fmt.Errorf("cannot remove network config: network is %s", m.Phase())
	}

	if err := os.Remove(m.networkConfigPath()); err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("remove network config: %w", err)
	}
	return nil
}

func (m *Machine) networkConfigPath() string {
	return filepath.Join(m.dataDir, networkConfigFileName)
}
