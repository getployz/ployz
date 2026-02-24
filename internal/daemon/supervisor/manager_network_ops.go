package supervisor

import (
	"context"
	"fmt"
	"log/slog"

	netctrl "ployz/internal/mesh"
	"ployz/pkg/sdk/defaults"
	"ployz/pkg/sdk/types"
)

func (m *Manager) ApplyNetworkSpec(ctx context.Context, spec types.NetworkSpec) (types.ApplyResult, error) {
	m.normalizeSpec(&spec)
	if spec.Network == "" {
		return types.ApplyResult{}, fmt.Errorf("network is required")
	}
	log := slog.With("component", "supervisor", "network", spec.Network)
	log.Info("apply network spec requested")

	// Stop the existing convergence worker before re-applying.
	if stopErr := m.engine.StopNetwork(spec.Network); stopErr != nil {
		log.Warn("failed to stop existing worker before apply", "err", stopErr)
	}

	// If this network already exists in persisted config, stop its currently
	// configured runtime first (WireGuard/Corrosion/Docker) before starting
	// again. This avoids apply/start races on shared runtime resources.
	persisted, exists, err := m.store.GetSpec(spec.Network)
	if err != nil {
		return types.ApplyResult{}, err
	}
	if exists {
		existingSpec := persisted.Spec
		m.normalizeSpec(&existingSpec)
		existingCfg, cfgErr := netctrl.ConfigFromSpec(existingSpec)
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

	// Start the convergence worker in-process.
	if err := m.engine.StartNetwork(m.ctx, spec); err != nil {
		return types.ApplyResult{}, fmt.Errorf("start convergence worker: %w", err)
	}

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
	if stopErr := m.engine.StopNetwork(network); stopErr != nil {
		log.Warn("failed to stop convergence worker", "err", stopErr)
	}

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

func (m *Manager) TriggerReconcile(ctx context.Context, network string) error {
	network = defaults.NormalizeNetwork(network)
	log := slog.With("component", "supervisor", "network", network)
	log.Debug("trigger reconcile requested")

	// Stop and restart the worker - forces a fresh reconciliation.
	if stopErr := m.engine.StopNetwork(network); stopErr != nil {
		log.Warn("failed to stop worker before reconcile", "err", stopErr)
	}

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
	if startErr := m.engine.StartNetwork(m.ctx, spec); startErr != nil {
		log.Warn("failed to restart convergence worker", "err", startErr)
	}
	log.Debug("worker restart requested")

	return nil
}

func (m *Manager) applyOnce(ctx context.Context, spec types.NetworkSpec) (types.ApplyResult, error) {
	cfg, err := netctrl.ConfigFromSpec(spec)
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
