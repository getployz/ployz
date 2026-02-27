package manager

import (
	"context"
	"fmt"
	"log/slog"
	"sort"
	"strings"

	"ployz/internal/deploy"
	"ployz/internal/engine"
	"ployz/internal/network"
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

	workloadCount, contexts, err := m.localManagedWorkloadSummary(ctx)
	if err != nil {
		return err
	}

	controlPlaneSummary := m.controlPlaneWorkloadSummary
	if controlPlaneSummary == nil {
		controlPlaneSummary = m.defaultControlPlaneManagedWorkloadSummary
	}
	controlPlaneCount, controlPlaneContexts, err := controlPlaneSummary(ctx, cfg)
	if err != nil {
		return err
	}

	if workloadCount > 0 || controlPlaneCount > 0 {
		details := make([]string, 0, 2)
		if workloadCount > 0 {
			details = append(details, buildWorkloadBlockDetail("local runtime", workloadCount, contexts))
		}
		if controlPlaneCount > 0 {
			details = append(details, buildWorkloadBlockDetail("control-plane", controlPlaneCount, controlPlaneContexts))
		}
		return fmt.Errorf("%w: %s", ErrNetworkDestroyHasWorkloads, strings.Join(details, "; "))
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

func (m *Manager) localManagedWorkloadSummary(ctx context.Context) (int, []string, error) {
	runtime := m.ctrl.ContainerRuntime()
	if runtime == nil {
		return 0, nil, fmt.Errorf("container runtime is unavailable")
	}

	entries, err := runtime.ContainerList(ctx, nil)
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

func (m *Manager) defaultControlPlaneManagedWorkloadSummary(ctx context.Context, cfg network.Config) (int, []string, error) {
	stores, err := m.deployStores(cfg)
	if err != nil {
		return 0, nil, err
	}
	if stores.Containers == nil {
		return 0, nil, fmt.Errorf("container store is unavailable")
	}
	if err := stores.Containers.EnsureContainerTable(ctx); err != nil {
		return 0, nil, fmt.Errorf("ensure deploy container table: %w", err)
	}

	rows, err := stores.Containers.ListContainers(ctx)
	if err != nil {
		return 0, nil, fmt.Errorf("list deploy containers: %w", err)
	}

	count, contexts := deployWorkloadContextSummary(rows)
	return count, contexts, nil
}

func deployWorkloadContextSummary(rows []deploy.ContainerRow) (int, []string) {
	if len(rows) == 0 {
		return 0, nil
	}

	contexts := make(map[string]struct{}, len(rows))
	for _, row := range rows {
		if len(contexts) >= maxWorkloadContexts {
			break
		}

		namespace := strings.TrimSpace(row.Namespace)
		deployID := strings.TrimSpace(row.DeployID)
		context := ""
		switch {
		case namespace != "":
			context = namespace
		case deployID != "":
			context = "deploy/" + deployID
		default:
			context = strings.TrimSpace(row.ID)
		}
		if context != "" {
			contexts[context] = struct{}{}
		}
	}

	summary := make([]string, 0, len(contexts))
	for context := range contexts {
		summary = append(summary, context)
	}
	sort.Strings(summary)

	return len(rows), summary
}

func buildWorkloadBlockDetail(source string, count int, contexts []string) string {
	detail := fmt.Sprintf("%s has %d managed workload containers", source, count)
	if len(contexts) == 0 {
		return detail
	}
	sortedContexts := append([]string(nil), contexts...)
	sort.Strings(sortedContexts)
	return fmt.Sprintf("%s (%s)", detail, strings.Join(sortedContexts, ", "))
}

func (m *Manager) defaultAttachedMachinesSummary(ctx context.Context, cfg network.Config) (int, []string, error) {
	rows, err := m.ctrl.ListMachines(ctx, cfg)
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
	if len(machineIDs) == 0 {
		return detail
	}
	sortedIDs := append([]string(nil), machineIDs...)
	sort.Strings(sortedIDs)
	return fmt.Sprintf("%s (%s)", detail, strings.Join(sortedIDs, ", "))
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
