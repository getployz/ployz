package manager

import (
	"context"
	"fmt"
	"log/slog"

	"ployz/internal/engine"
	"ployz/internal/network"
	"ployz/pkg/sdk/types"
)

func (m *Manager) ApplyNetworkSpec(ctx context.Context, spec types.NetworkSpec) (types.ApplyResult, error) {
	m.normalizeSpec(&spec)
	if spec.Network == "" {
		return types.ApplyResult{}, fmt.Errorf("network is required")
	}
	log := slog.With("component", "manager", "network", spec.Network)
	log.Info("apply network spec requested")

	// Stop the existing supervisor loop before re-applying.
	if stopErr := m.engine.Stop(); stopErr != nil {
		log.Warn("failed to stop existing supervisor loop before apply", "err", stopErr)
	}

	// If this network already exists in persisted config, stop its currently
	// configured runtime first (WireGuard/Corrosion/Docker) before starting
	// again. This avoids apply/start races on shared runtime resources.
	persisted, exists, err := m.store.GetSpec()
	if err != nil {
		return types.ApplyResult{}, err
	}
	if exists {
		existingSpec := persisted.Spec
		m.normalizeSpec(&existingSpec)
		existingCfg, cfgErr := network.ConfigFromSpec(existingSpec)
		if cfgErr != nil {
			log.Warn("failed to resolve existing config before apply", "err", cfgErr)
		} else {
			if _, stopErr := m.ctrl.Stop(ctx, existingCfg, false); stopErr != nil {
				log.Warn("failed to stop existing runtime before apply", "err", stopErr)
			}
		}
	}

	result, err := m.applyOnce(ctx, spec)
	if err != nil {
		log.Error("apply network spec failed", "err", err)
		return types.ApplyResult{}, err
	}
	if err := m.store.SaveSpec(spec, true); err != nil {
		return types.ApplyResult{}, err
	}

	// Start the supervisor loop in-process.
	if err := m.engine.Start(m.ctx, spec); err != nil {
		return types.ApplyResult{}, fmt.Errorf("start supervisor loop: %w", err)
	}

	phase, _ := m.engine.Status()
	result.SupervisorRunning = supervisorPhaseRunning(phase)
	log.Info("network apply complete", "supervisor_running", result.SupervisorRunning)

	return result, nil
}

func supervisorPhaseRunning(phase engine.SupervisorPhase) bool {
	switch phase {
	case engine.SupervisorStarting, engine.SupervisorRunning, engine.SupervisorDegraded, engine.SupervisorBackoff:
		return true
	default:
		return false
	}
}

func (m *Manager) DisableNetwork(ctx context.Context, purge bool) error {
	spec, cfg, err := m.resolveConfig()
	if err != nil {
		return err
	}
	networkName := spec.Network
	log := slog.With("component", "manager", "network", networkName, "purge", purge)
	log.Info("disable requested")

	// Stop the supervisor loop first.
	if stopErr := m.engine.Stop(); stopErr != nil {
		log.Warn("failed to stop supervisor loop", "err", stopErr)
	}

	if _, err := m.ctrl.Stop(ctx, cfg, purge); err != nil {
		return err
	}

	if purge {
		if err := m.store.DeleteSpec(); err != nil {
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

func (m *Manager) applyOnce(ctx context.Context, spec types.NetworkSpec) (types.ApplyResult, error) {
	cfg, err := network.ConfigFromSpec(spec)
	if err != nil {
		return types.ApplyResult{}, err
	}

	out, err := m.ctrl.Start(ctx, cfg)
	if err != nil {
		return types.ApplyResult{}, err
	}

	return types.ApplyResult{
		Network:                 out.Network,
		NetworkCIDR:             out.NetworkCIDR.String(),
		Subnet:                  out.Subnet.String(),
		ManagementIP:            out.Management.String(),
		WGInterface:             out.WGInterface,
		WGPort:                  out.WGPort,
		AdvertiseEndpoint:       out.AdvertiseEndpoint,
		CorrosionName:           out.CorrosionName,
		CorrosionAPIAddr:        out.CorrosionAPIAddr.String(),
		CorrosionGossipAddrPort: out.CorrosionGossipAddrPort.String(),
		DockerNetwork:           out.DockerNetwork,
	}, nil
}
