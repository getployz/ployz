package manager

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sort"
	"strings"
	"sync"
	"time"

	compose "github.com/compose-spec/compose-go/v2/types"

	"ployz/internal/adapter/corrosion"
	"ployz/internal/check"
	"ployz/internal/deploy"
	"ployz/internal/network"
	"ployz/pkg/sdk/types"
)

const (
	deployEventBufferCapacity = 256
	defaultHealthPollInterval = 250 * time.Millisecond
	defaultHealthTimeout      = 30 * time.Second
)

func (m *Manager) PlanDeploy(ctx context.Context, namespace string, composeSpec []byte) (types.DeployPlan, error) {
	plan, _, err := m.buildDeployPlan(ctx, namespace, composeSpec)
	if err != nil {
		return types.DeployPlan{}, err
	}
	return deployPlanToSDK(plan)
}

func (m *Manager) ApplyDeploy(ctx context.Context, namespace string, composeSpec []byte, events chan<- types.DeployProgressEvent) (types.DeployResult, error) {
	plan, cfg, err := m.buildDeployPlan(ctx, namespace, composeSpec)
	if err != nil {
		return types.DeployResult{}, err
	}

	identity, err := m.GetIdentity(ctx)
	if err != nil {
		return types.DeployResult{}, fmt.Errorf("read local identity: %w", err)
	}
	machineID := strings.TrimSpace(identity.ID)
	if machineID == "" {
		return types.DeployResult{}, fmt.Errorf("machine id is required")
	}

	store := corrosion.NewStore(cfg.CorrosionAPIAddr, cfg.CorrosionAPIToken)
	if err := store.EnsureContainerTable(ctx); err != nil {
		return types.DeployResult{}, fmt.Errorf("ensure deploy container table: %w", err)
	}
	if err := store.EnsureDeploymentTable(ctx); err != nil {
		return types.DeployResult{}, fmt.Errorf("ensure deploy deployment table: %w", err)
	}

	runtime := m.ctrl.ContainerRuntime()
	check.Assert(runtime != nil, "Manager.ApplyDeploy: container runtime must not be nil")
	if runtime == nil {
		return types.DeployResult{}, fmt.Errorf("container runtime is unavailable")
	}

	clock := m.ctrl.Clock()
	if clock == nil {
		clock = network.RealClock{}
	}

	stores := deploy.Stores{Containers: store, Deployments: store}
	health := runtimeHealthChecker{runtime: runtime}
	stateReader := localStateReader{runtime: runtime, machineID: machineID}

	internalEvents := make(chan deploy.ProgressEvent, deployEventBufferCapacity)
	var wg sync.WaitGroup
	if events != nil {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for ev := range internalEvents {
				sdkEvent := deployProgressEventToSDK(ev)
				select {
				case events <- sdkEvent:
				default:
				}
			}
		}()
	}

	applyResult, applyErr := deploy.ApplyPlan(ctx, runtime, stores, health, stateReader, plan, machineID, clock, internalEvents)
	close(internalEvents)
	wg.Wait()

	out := applyResultToSDK(applyResult)
	if applyErr != nil {
		out.Status = deploy.DeployFailed.String()
		out.ErrorReason = deploy.DeployErrorReasonUnknown.String()
		var deployErr *deploy.DeployError
		if errors.As(applyErr, &deployErr) {
			out.ErrorMessage = deployErr.Message
			if out.ErrorMessage == "" {
				out.ErrorMessage = deployErr.Error()
			}
			out.ErrorPhase = deployErr.Phase.String()
			out.ErrorReason = deployErr.Reason.String()
			out.ErrorTier = deployErr.Tier
		}
		return out, applyErr
	}

	out.Status = deploy.DeploySucceeded.String()
	return out, nil
}

func (m *Manager) ListDeployments(ctx context.Context, namespace string) ([]types.DeploymentEntry, error) {
	namespace = strings.TrimSpace(namespace)
	if namespace == "" {
		return nil, fmt.Errorf("namespace is required")
	}

	_, cfg, err := m.resolveConfig()
	if err != nil {
		return nil, err
	}
	store := corrosion.NewStore(cfg.CorrosionAPIAddr, cfg.CorrosionAPIToken)
	if err := store.EnsureDeploymentTable(ctx); err != nil {
		return nil, fmt.Errorf("ensure deploy deployment table: %w", err)
	}

	rows, err := store.ListByNamespace(ctx, namespace)
	if err != nil {
		return nil, fmt.Errorf("list deployments for namespace %q: %w", namespace, err)
	}

	out := make([]types.DeploymentEntry, 0, len(rows))
	for _, row := range rows {
		out = append(out, deploymentRowToSDK(row))
	}
	return out, nil
}

func (m *Manager) RemoveNamespace(ctx context.Context, namespace string) error {
	namespace = strings.TrimSpace(namespace)
	if namespace == "" {
		return fmt.Errorf("namespace is required")
	}

	_, cfg, err := m.resolveConfig()
	if err != nil {
		return err
	}

	identity, err := m.GetIdentity(ctx)
	if err != nil {
		return fmt.Errorf("read local identity: %w", err)
	}
	machineID := strings.TrimSpace(identity.ID)

	store := corrosion.NewStore(cfg.CorrosionAPIAddr, cfg.CorrosionAPIToken)
	if err := store.EnsureContainerTable(ctx); err != nil {
		return fmt.Errorf("ensure deploy container table: %w", err)
	}

	runtime := m.ctrl.ContainerRuntime()
	check.Assert(runtime != nil, "Manager.RemoveNamespace: container runtime must not be nil")
	if runtime == nil {
		return fmt.Errorf("container runtime is unavailable")
	}

	return deploy.RemoveNamespace(ctx, runtime, deploy.Stores{Containers: store}, namespace, machineID)
}

func (m *Manager) ReadContainerState(ctx context.Context, namespace string) ([]types.ContainerState, error) {
	namespace = strings.TrimSpace(namespace)
	if namespace == "" {
		return nil, fmt.Errorf("namespace is required")
	}

	_, _, err := m.resolveConfig()
	if err != nil {
		return nil, err
	}

	runtime := m.ctrl.ContainerRuntime()
	check.Assert(runtime != nil, "Manager.ReadContainerState: container runtime must not be nil")
	if runtime == nil {
		return nil, fmt.Errorf("container runtime is unavailable")
	}

	entries, err := runtime.ContainerList(ctx, map[string]string{"ployz.namespace": namespace})
	if err != nil {
		return nil, fmt.Errorf("list runtime containers for namespace %q: %w", namespace, err)
	}

	out := make([]types.ContainerState, 0, len(entries))
	for _, entry := range entries {
		out = append(out, types.ContainerState{
			ContainerName: entry.Name,
			Image:         entry.Image,
			Running:       entry.Running,
			Healthy:       entry.Running,
		})
	}
	return out, nil
}

func (m *Manager) buildDeployPlan(ctx context.Context, namespace string, composeSpec []byte) (deploy.DeployPlan, network.Config, error) {
	namespace = strings.TrimSpace(namespace)
	if namespace == "" {
		return deploy.DeployPlan{}, network.Config{}, fmt.Errorf("namespace is required")
	}
	if len(composeSpec) == 0 {
		return deploy.DeployPlan{}, network.Config{}, fmt.Errorf("compose spec is required")
	}

	status, err := m.GetStatus(ctx)
	if err != nil {
		return deploy.DeployPlan{}, network.Config{}, fmt.Errorf("read runtime status: %w", err)
	}
	if blockers := status.ServiceBlockerIssues(); len(blockers) > 0 {
		return deploy.DeployPlan{}, network.Config{}, fmt.Errorf("%w: %s", ErrRuntimeNotReadyForServices, joinStatusIssues(blockers))
	}

	project, err := deploy.LoadSpec(ctx, composeSpec, namespace)
	if err != nil {
		return deploy.DeployPlan{}, network.Config{}, fmt.Errorf("load compose spec: %w", err)
	}

	spec, cfg, err := m.resolveConfig()
	if err != nil {
		return deploy.DeployPlan{}, network.Config{}, err
	}
	networkName := strings.TrimSpace(spec.Network)
	if networkName == "" {
		return deploy.DeployPlan{}, network.Config{}, fmt.Errorf("network is required")
	}

	incoming, err := composeProjectToDeploySpec(project, namespace, networkName)
	if err != nil {
		return deploy.DeployPlan{}, network.Config{}, err
	}

	store := corrosion.NewStore(cfg.CorrosionAPIAddr, cfg.CorrosionAPIToken)
	if err := store.EnsureContainerTable(ctx); err != nil {
		return deploy.DeployPlan{}, network.Config{}, fmt.Errorf("ensure deploy container table: %w", err)
	}
	current, err := store.ListContainersByNamespace(ctx, namespace)
	if err != nil {
		return deploy.DeployPlan{}, network.Config{}, fmt.Errorf("list containers for namespace %q: %w", namespace, err)
	}

	machines, err := m.ctrl.ListMachines(ctx, cfg)
	if err != nil {
		return deploy.DeployPlan{}, network.Config{}, fmt.Errorf("list machines: %w", err)
	}
	machineInfo := make([]deploy.MachineInfo, 0, len(machines))
	for _, machine := range machines {
		if strings.TrimSpace(machine.ID) == "" {
			continue
		}
		machineInfo = append(machineInfo, deploy.MachineInfo{ID: machine.ID, Labels: map[string]string{}})
	}

	assignments, err := deploy.Schedule(namespace, incoming.Services, machineInfo, current)
	if err != nil {
		return deploy.DeployPlan{}, network.Config{}, fmt.Errorf("schedule deploy: %w", err)
	}

	plan, err := deploy.PlanDeploy(incoming, current, assignments)
	if err != nil {
		return deploy.DeployPlan{}, network.Config{}, fmt.Errorf("plan deploy: %w", err)
	}
	return plan, cfg, nil
}

func joinStatusIssues(issues []types.StatusIssue) string {
	parts := make([]string, 0, len(issues))
	for _, issue := range issues {
		message := strings.TrimSpace(issue.Message)
		if message == "" {
			continue
		}
		component := strings.TrimSpace(issue.Component)
		if component == "" {
			parts = append(parts, message)
			continue
		}
		parts = append(parts, component+": "+message)
	}
	if len(parts) == 0 {
		return "runtime has unresolved blockers"
	}
	return strings.Join(parts, "; ")
}

func composeProjectToDeploySpec(project *compose.Project, namespace, networkName string) (deploy.DeploySpec, error) {
	check.Assert(project != nil, "composeProjectToDeploySpec: project must not be nil")
	if project == nil {
		return deploy.DeploySpec{}, fmt.Errorf("compose project is required")
	}

	serviceNames := project.ServiceNames()
	sort.Strings(serviceNames)
	services := make([]deploy.ServiceDeployConfig, 0, len(serviceNames))

	for _, serviceName := range serviceNames {
		svc, err := project.GetService(serviceName)
		if err != nil {
			return deploy.DeploySpec{}, fmt.Errorf("get service %q: %w", serviceName, err)
		}

		placement := deploy.PlacementReplicated
		replicas := 0
		constraints := []string(nil)
		deployLabels := map[string]string{}
		updateConfig := deploy.UpdateConfig{}

		if svc.Deploy != nil {
			mode := strings.TrimSpace(svc.Deploy.Mode)
			switch mode {
			case "", string(deploy.PlacementReplicated):
				placement = deploy.PlacementReplicated
			case string(deploy.PlacementGlobal):
				placement = deploy.PlacementGlobal
			default:
				return deploy.DeploySpec{}, fmt.Errorf("service %q: unsupported placement mode %q", serviceName, mode)
			}

			if svc.Deploy.Replicas != nil {
				replicas = *svc.Deploy.Replicas
			}
			if len(svc.Deploy.Placement.Constraints) > 0 {
				constraints = append([]string(nil), svc.Deploy.Placement.Constraints...)
			}
			if len(svc.Deploy.Labels) > 0 {
				deployLabels = make(map[string]string, len(svc.Deploy.Labels))
				for key, value := range svc.Deploy.Labels {
					deployLabels[key] = value
				}
			}
			if svc.Deploy.UpdateConfig != nil {
				if svc.Deploy.UpdateConfig.Parallelism != nil {
					updateConfig.Parallelism = int(*svc.Deploy.UpdateConfig.Parallelism)
				}
				updateConfig.Order = strings.TrimSpace(svc.Deploy.UpdateConfig.Order)
				updateConfig.FailureAction = strings.TrimSpace(svc.Deploy.UpdateConfig.FailureAction)
			}
		}

		if svc.Deploy == nil && svc.Scale != nil {
			replicas = *svc.Scale
		}
		if replicas < 0 {
			return deploy.DeploySpec{}, fmt.Errorf("service %q: replicas must be >= 0", serviceName)
		}

		dependsOn := make([]string, 0, len(svc.DependsOn))
		for dependency := range svc.DependsOn {
			dependsOn = append(dependsOn, dependency)
		}
		sort.Strings(dependsOn)

		services = append(services, deploy.ServiceDeployConfig{
			Spec:         deploy.NormalizeServiceSpec(svc),
			Placement:    placement,
			Replicas:     replicas,
			Constraints:  constraints,
			DeployLabels: deployLabels,
			UpdateConfig: updateConfig,
			DependsOn:    dependsOn,
		})
	}

	return deploy.DeploySpec{
		Namespace: namespace,
		Network:   networkName,
		Services:  services,
	}, nil
}

func deployPlanToSDK(plan deploy.DeployPlan) (types.DeployPlan, error) {
	out := types.DeployPlan{
		Namespace: plan.Namespace,
		DeployID:  plan.DeployID,
		Tiers:     make([]types.DeployTier, 0, len(plan.Tiers)),
	}

	for _, tier := range plan.Tiers {
		services := make([]types.DeployServicePlan, 0, len(tier.Services))
		for _, service := range tier.Services {
			converted, err := deployServicePlanToSDK(service)
			if err != nil {
				return types.DeployPlan{}, err
			}
			services = append(services, converted)
		}
		out.Tiers = append(out.Tiers, types.DeployTier{Services: services})
	}

	return out, nil
}

func deployServicePlanToSDK(plan deploy.ServicePlan) (types.DeployServicePlan, error) {
	upToDate, err := deployEntriesToSDK(plan.UpToDate)
	if err != nil {
		return types.DeployServicePlan{}, err
	}
	specUpdates, err := deployEntriesToSDK(plan.NeedsSpecUpdate)
	if err != nil {
		return types.DeployServicePlan{}, err
	}
	updates, err := deployEntriesToSDK(plan.NeedsUpdate)
	if err != nil {
		return types.DeployServicePlan{}, err
	}
	recreates, err := deployEntriesToSDK(plan.NeedsRecreate)
	if err != nil {
		return types.DeployServicePlan{}, err
	}
	creates, err := deployEntriesToSDK(plan.Create)
	if err != nil {
		return types.DeployServicePlan{}, err
	}
	removes, err := deployEntriesToSDK(plan.Remove)
	if err != nil {
		return types.DeployServicePlan{}, err
	}

	out := types.DeployServicePlan{
		Name:            plan.Name,
		UpToDate:        upToDate,
		NeedsSpecUpdate: specUpdates,
		NeedsUpdate:     updates,
		NeedsRecreate:   recreates,
		Create:          creates,
		Remove:          removes,
		UpdateConfig: types.DeployUpdateConfig{
			Order:         plan.UpdateConfig.Order,
			Parallelism:   plan.UpdateConfig.Parallelism,
			FailureAction: plan.UpdateConfig.FailureAction,
		},
	}
	if plan.HealthCheck != nil {
		out.HealthCheck = &types.DeployHealthCheck{
			Test:        append([]string(nil), plan.HealthCheck.Test...),
			Interval:    plan.HealthCheck.Interval,
			Timeout:     plan.HealthCheck.Timeout,
			Retries:     plan.HealthCheck.Retries,
			StartPeriod: plan.HealthCheck.StartPeriod,
		}
	}
	return out, nil
}

func deployEntriesToSDK(entries []deploy.PlanEntry) ([]types.DeployPlanEntry, error) {
	out := make([]types.DeployPlanEntry, 0, len(entries))
	for _, entry := range entries {
		specJSON, err := json.Marshal(entry.Spec)
		if err != nil {
			return nil, fmt.Errorf("marshal service spec: %w", err)
		}

		currentRowJSON := ""
		if entry.CurrentRow != nil {
			encoded, err := json.Marshal(entry.CurrentRow)
			if err != nil {
				return nil, fmt.Errorf("marshal current container row: %w", err)
			}
			currentRowJSON = string(encoded)
		}

		out = append(out, types.DeployPlanEntry{
			MachineID:      entry.MachineID,
			ContainerName:  entry.ContainerName,
			SpecJSON:       string(specJSON),
			CurrentRowJSON: currentRowJSON,
			ReasonCode:     entry.ReasonCode.String(),
			Reason:         entry.Reason,
		})
	}
	return out, nil
}

func applyResultToSDK(result deploy.ApplyResult) types.DeployResult {
	out := types.DeployResult{
		Namespace: result.Namespace,
		DeployID:  result.DeployID,
		Tiers:     make([]types.DeployTierResult, 0, len(result.Tiers)),
	}
	for _, tier := range result.Tiers {
		containers := make([]types.DeployContainerResult, 0, len(tier.Containers))
		for _, container := range tier.Containers {
			containers = append(containers, types.DeployContainerResult{
				MachineID:     container.MachineID,
				ContainerName: container.ContainerName,
				Expected:      container.Expected,
				Actual:        container.Actual,
				Match:         container.Match,
			})
		}
		out.Tiers = append(out.Tiers, types.DeployTierResult{
			Name:       tier.Name,
			Status:     tier.Status.String(),
			Containers: containers,
		})
	}
	return out
}

func deploymentRowToSDK(row deploy.DeploymentRow) types.DeploymentEntry {
	labels := make(map[string]string, len(row.Labels))
	for key, value := range row.Labels {
		labels[key] = value
	}
	machineIDs := append([]string(nil), row.MachineIDs...)
	return types.DeploymentEntry{
		ID:             row.ID,
		Namespace:      row.Namespace,
		Status:         row.Status.String(),
		Owner:          row.Owner,
		OwnerHeartbeat: row.OwnerHeartbeat,
		Labels:         labels,
		MachineIDs:     machineIDs,
		Version:        row.Version,
		CreatedAt:      row.CreatedAt,
		UpdatedAt:      row.UpdatedAt,
	}
}

func deployProgressEventToSDK(ev deploy.ProgressEvent) types.DeployProgressEvent {
	return types.DeployProgressEvent{
		Type:      ev.Type,
		Tier:      ev.Tier,
		Service:   ev.Service,
		MachineID: ev.MachineID,
		Container: ev.Container,
		Message:   ev.Message,
	}
}

type runtimeHealthChecker struct {
	runtime network.ContainerRuntime
}

func (h runtimeHealthChecker) WaitHealthy(ctx context.Context, containerName string, cfg deploy.HealthCheck) error {
	check.Assert(h.runtime != nil, "runtimeHealthChecker.WaitHealthy: runtime must not be nil")
	if h.runtime == nil {
		return fmt.Errorf("container runtime is required")
	}

	if cfg.StartPeriod > 0 {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(cfg.StartPeriod):
		}
	}

	timeout := cfg.Timeout
	if timeout <= 0 {
		timeout = defaultHealthTimeout
	}
	checkCtx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	interval := cfg.Interval
	if interval <= 0 {
		interval = defaultHealthPollInterval
	}
	attempts := cfg.Retries
	if attempts <= 0 {
		attempts = 1
	}

	var lastErr error
	for attempt := 0; attempt < attempts; attempt++ {
		info, err := h.runtime.ContainerInspect(checkCtx, containerName)
		if err == nil && info.Exists && info.Running {
			return nil
		}
		if err != nil {
			lastErr = err
		} else {
			lastErr = fmt.Errorf("container %q not running", containerName)
		}

		if attempt == attempts-1 {
			break
		}

		select {
		case <-checkCtx.Done():
			if lastErr != nil {
				return fmt.Errorf("wait healthy %q: %w", containerName, lastErr)
			}
			return checkCtx.Err()
		case <-time.After(interval):
		}
	}

	if lastErr == nil {
		lastErr = fmt.Errorf("container %q did not become healthy", containerName)
	}
	return fmt.Errorf("wait healthy %q: %w", containerName, lastErr)
}

type localStateReader struct {
	runtime   network.ContainerRuntime
	machineID string
}

func (r localStateReader) ReadMachineState(ctx context.Context, machineID, namespace string) ([]deploy.ContainerState, error) {
	check.Assert(r.runtime != nil, "localStateReader.ReadMachineState: runtime must not be nil")
	if r.runtime == nil {
		return nil, fmt.Errorf("container runtime is required")
	}
	if machineID != "" && r.machineID != "" && machineID != r.machineID {
		return nil, fmt.Errorf("state reader only supports local machine %q, got %q", r.machineID, machineID)
	}

	entries, err := r.runtime.ContainerList(ctx, map[string]string{"ployz.namespace": namespace})
	if err != nil {
		return nil, fmt.Errorf("list runtime containers for namespace %q: %w", namespace, err)
	}

	out := make([]deploy.ContainerState, 0, len(entries))
	for _, entry := range entries {
		out = append(out, deploy.ContainerState{
			ContainerName: entry.Name,
			Image:         entry.Image,
			Running:       entry.Running,
			Healthy:       entry.Running,
		})
	}
	return out, nil
}
