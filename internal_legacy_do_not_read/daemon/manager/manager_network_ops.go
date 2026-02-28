package manager

import (
	"context"
	"fmt"
	"log/slog"
	"sort"
	"strings"

	"ployz/internal/daemon/convergence"
	"ployz/internal/daemon/overlay"
	"ployz/pkg/sdk/types"
)

const (
	managedLabelNamespace = "ployz.namespace"
	managedLabelDeployID  = "ployz.deploy_id"
	maxWorkloadContexts   = 3
	maxAttachedMachines   = 5
)

func (m *Manager) ApplyNetworkSpec(ctx context.Context, spec types.NetworkSpec) (types.ApplyResult, error) {
	m.normalizeSpec(&spec)
	if spec.Network == "" {
		return types.ApplyResult{}, fmt.Errorf("network is required")
	}
	log := slog.With("component", "manager", "network", spec.Network)
	log.Info("apply network spec requested")

	// Stop the existing supervisor loop before re-applying.
	if stopErr := m.convergence.Stop(); stopErr != nil {
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
		existingCfg, cfgErr := overlay.ConfigFromSpec(existingSpec)
		if cfgErr != nil {
			log.Warn("failed to resolve existing config before apply", "err", cfgErr)
		} else {
			if _, stopErr := m.overlay.Stop(ctx, existingCfg, false); stopErr != nil {
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
	if err := m.convergence.Start(m.ctx, spec); err != nil {
		return types.ApplyResult{}, fmt.Errorf("start supervisor loop: %w", err)
	}

	phase, _ := m.convergence.Status()
	result.SupervisorRunning = supervisorPhaseRunning(phase)
	log.Info("network apply complete", "supervisor_running", result.SupervisorRunning)

	return result, nil
}

func supervisorPhaseRunning(phase convergence.SupervisorPhase) bool {
	switch phase {
	case convergence.SupervisorStarting, convergence.SupervisorRunning, convergence.SupervisorDegraded, convergence.SupervisorBackoff:
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

	workloadCount, contexts, err := m.localManagedWorkloadSummary(ctx)
	if err != nil {
		return err
	}
	if workloadCount > 0 {
		return fmt.Errorf("%w: %s", ErrNetworkDestroyHasWorkloads, buildWorkloadBlockDetail("local runtime", workloadCount, contexts))
	}

	attachedSummary := m.attachedMachinesSummary
	if attachedSummary == nil {
		attachedSummary = m.defaultAttachedMachinesSummary
	}
	attachedCount, machineIDs, err := attachedSummary(ctx, cfg)
	if err != nil {
		return err
	}
	if attachedCount > 0 {
		return fmt.Errorf("%w: %s", ErrNetworkDestroyHasMachines, buildAttachedMachinesBlockDetail(attachedCount, machineIDs))
	}

	// Stop the supervisor loop first.
	if stopErr := m.convergence.Stop(); stopErr != nil {
		log.Warn("failed to stop supervisor loop", "err", stopErr)
	}

	if _, err := m.overlay.Stop(ctx, cfg, purge); err != nil {
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

func (m *Manager) localManagedWorkloadSummary(ctx context.Context) (int, []string, error) {
	entries, err := m.overlay.ListContainers(ctx, nil)
	if err != nil {
		return 0, nil, fmt.Errorf("list local runtime containers: %w", err)
	}

	count := 0
	contexts := make(map[string]struct{}, len(entries))
	for _, entry := range entries {
		namespace := strings.TrimSpace(entry.Labels[managedLabelNamespace])
		deployID := strings.TrimSpace(entry.Labels[managedLabelDeployID])
		if namespace == "" || deployID == "" {
			continue
		}
		count++
		if len(contexts) < maxWorkloadContexts {
			contexts[namespace] = struct{}{}
		}
	}

	summary := make([]string, 0, len(contexts))
	for context := range contexts {
		summary = append(summary, context)
	}
	sort.Strings(summary)

	return count, summary, nil
}

func buildWorkloadBlockDetail(source string, count int, contexts []string) string {
	detail := fmt.Sprintf("%s has %d managed workload containers", source, count)
	return appendDetailValues(detail, contexts)
}

func (m *Manager) defaultAttachedMachinesSummary(ctx context.Context, cfg overlay.Config) (int, []string, error) {
	rows, err := m.membership.ListMachines(ctx, cfg)
	if err != nil {
		return 0, nil, fmt.Errorf("list attached machines: %w", err)
	}

	localID := ""
	identity, idErr := m.GetIdentity(ctx)
	if idErr == nil {
		localID = strings.TrimSpace(identity.ID)
	}

	machineSet := make(map[string]struct{}, len(rows))
	for _, row := range rows {
		id := strings.TrimSpace(row.ID)
		if id == "" {
			continue
		}
		if localID != "" && id == localID {
			continue
		}
		machineSet[id] = struct{}{}
	}

	machineIDs := make([]string, 0, len(machineSet))
	for id := range machineSet {
		machineIDs = append(machineIDs, id)
	}
	sort.Strings(machineIDs)
	if len(machineIDs) > maxAttachedMachines {
		machineIDs = machineIDs[:maxAttachedMachines]
	}

	return len(machineSet), machineIDs, nil
}

func buildAttachedMachinesBlockDetail(count int, machineIDs []string) string {
	detail := fmt.Sprintf("network has %d attached machines", count)
	return appendDetailValues(detail, machineIDs)
}

func appendDetailValues(detail string, values []string) string {
	if len(values) == 0 {
		return detail
	}

	sortedValues := append([]string(nil), values...)
	sort.Strings(sortedValues)

	return fmt.Sprintf("%s (%s)", detail, strings.Join(sortedValues, ", "))
}

func (m *Manager) applyOnce(ctx context.Context, spec types.NetworkSpec) (types.ApplyResult, error) {
	cfg, err := overlay.ConfigFromSpec(spec)
	if err != nil {
		return types.ApplyResult{}, err
	}

	out, err := m.overlay.Start(ctx, cfg)
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
