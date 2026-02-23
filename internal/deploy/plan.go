package deploy

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"sort"
	"strings"
)

const (
	updateOrderStartFirst = "start-first"
	updateOrderStopFirst  = "stop-first"
)

// PlanDeploy computes the minimal set of operations to transition from current
// state to the incoming spec, grouped into dependency tiers.
func PlanDeploy(
	incoming DeploySpec,
	current []ContainerRow,
	schedule map[string][]MachineAssignment,
) (DeployPlan, error) {
	tiers, err := TopologicalSort(incoming.Services)
	if err != nil {
		return DeployPlan{}, err
	}

	currentByServiceMachine := make(map[string]map[string][]ContainerRow)
	currentByService := make(map[string][]ContainerRow)
	currentCountByService := make(map[string]int)
	for _, row := range current {
		currentByService[row.Service] = append(currentByService[row.Service], row)
		currentCountByService[row.Service]++
		if currentByServiceMachine[row.Service] == nil {
			currentByServiceMachine[row.Service] = make(map[string][]ContainerRow)
		}
		currentByServiceMachine[row.Service][row.MachineID] = append(currentByServiceMachine[row.Service][row.MachineID], row)
	}
	for service := range currentByServiceMachine {
		for machineID, rows := range currentByServiceMachine[service] {
			sort.Slice(rows, func(i, j int) bool {
				return rows[i].ContainerName < rows[j].ContainerName
			})
			currentByServiceMachine[service][machineID] = rows
		}
	}

	targetCountByService := make(map[string]int, len(schedule))
	for service, assignments := range schedule {
		targetCountByService[service] = len(assignments)
	}

	usedRows := make(map[string]bool, len(current))
	planTiers := make([]Tier, 0, len(tiers))
	incomingServices := make(map[string]bool, len(incoming.Services))

	for _, tierServices := range tiers {
		tier := Tier{Services: make([]ServicePlan, 0, len(tierServices))}
		for _, svc := range tierServices {
			incomingServices[svc.Spec.Name] = true

			servicePlan := ServicePlan{
				Name:         svc.Spec.Name,
				UpdateConfig: svc.UpdateConfig,
				HealthCheck:  svc.Spec.HealthCheck,
			}

			assignments := append([]MachineAssignment(nil), schedule[svc.Spec.Name]...)
			sort.Slice(assignments, func(i, j int) bool {
				if assignments[i].MachineID != assignments[j].MachineID {
					return assignments[i].MachineID < assignments[j].MachineID
				}
				return assignments[i].ContainerName < assignments[j].ContainerName
			})

			for _, assignment := range assignments {
				matched, ok := popCurrentRow(currentByServiceMachine, svc.Spec.Name, assignment.MachineID)
				if !ok {
					servicePlan.Create = append(servicePlan.Create, PlanEntry{
						MachineID:     assignment.MachineID,
						ContainerName: assignment.ContainerName,
						Spec:          svc.Spec,
						Reason:        createReason(svc.Spec.Name, currentCountByService, targetCountByService),
					})
					continue
				}

				usedRows[matched.ID] = true
				currentSpec, parseErr := decodeServiceSpec(matched.SpecJSON)
				if parseErr != nil {
					servicePlan.NeedsRecreate = append(servicePlan.NeedsRecreate, PlanEntry{
						MachineID:     assignment.MachineID,
						ContainerName: assignment.ContainerName,
						Spec:          svc.Spec,
						CurrentRow:    cloneContainerRowPtr(matched),
						Reason:        fmt.Sprintf("current spec decode failed: %v", parseErr),
					})
					continue
				}

				kind, reason := classifyPlannedChange(currentSpec, svc.Spec, matched.SpecJSON)
				entry := PlanEntry{
					MachineID:  assignment.MachineID,
					Spec:       svc.Spec,
					CurrentRow: cloneContainerRowPtr(matched),
					Reason:     reason,
				}
				switch kind {
				case UpToDate:
					entry.ContainerName = matched.ContainerName
					servicePlan.UpToDate = append(servicePlan.UpToDate, entry)
				case NeedsSpecUpdate:
					entry.ContainerName = matched.ContainerName
					servicePlan.NeedsSpecUpdate = append(servicePlan.NeedsSpecUpdate, entry)
				case NeedsUpdate:
					entry.ContainerName = matched.ContainerName
					servicePlan.NeedsUpdate = append(servicePlan.NeedsUpdate, entry)
				case NeedsRecreate:
					entry.ContainerName = assignment.ContainerName
					servicePlan.NeedsRecreate = append(servicePlan.NeedsRecreate, entry)
				default:
					entry.ContainerName = assignment.ContainerName
					servicePlan.NeedsRecreate = append(servicePlan.NeedsRecreate, entry)
				}
			}

			for _, leftover := range currentByServiceMachine[svc.Spec.Name] {
				for _, row := range leftover {
					if usedRows[row.ID] {
						continue
					}
					usedRows[row.ID] = true
					servicePlan.Remove = append(servicePlan.Remove, PlanEntry{
						MachineID:     row.MachineID,
						ContainerName: row.ContainerName,
						CurrentRow:    cloneContainerRowPtr(row),
						Reason:        removeReason(svc.Spec.Name, currentCountByService, targetCountByService),
					})
				}
			}

			tier.Services = append(tier.Services, servicePlan)
		}
		planTiers = append(planTiers, tier)
	}

	removedServices := make(map[string][]ContainerRow)
	for serviceName, rows := range currentByService {
		if incomingServices[serviceName] {
			continue
		}
		for _, row := range rows {
			if usedRows[row.ID] {
				continue
			}
			usedRows[row.ID] = true
			removedServices[serviceName] = append(removedServices[serviceName], row)
		}
	}
	if len(removedServices) > 0 {
		serviceNames := make([]string, 0, len(removedServices))
		for serviceName := range removedServices {
			serviceNames = append(serviceNames, serviceName)
		}
		sort.Strings(serviceNames)

		tier := Tier{Services: make([]ServicePlan, 0, len(serviceNames))}
		for _, serviceName := range serviceNames {
			rows := removedServices[serviceName]
			sort.Slice(rows, func(i, j int) bool {
				if rows[i].MachineID != rows[j].MachineID {
					return rows[i].MachineID < rows[j].MachineID
				}
				return rows[i].ContainerName < rows[j].ContainerName
			})

			sp := ServicePlan{Name: serviceName}
			for _, row := range rows {
				sp.Remove = append(sp.Remove, PlanEntry{
					MachineID:     row.MachineID,
					ContainerName: row.ContainerName,
					CurrentRow:    cloneContainerRowPtr(row),
					Reason:        "service removed",
				})
			}
			tier.Services = append(tier.Services, sp)
		}
		planTiers = append(planTiers, tier)
	}

	plan := DeployPlan{
		Namespace: incoming.Namespace,
		DeployID:  deterministicDeployID(incoming, schedule),
		Tiers:     planTiers,
	}

	return plan, nil
}

// TopologicalSort sorts services into dependency tiers based on DependsOn.
func TopologicalSort(services []ServiceDeployConfig) ([][]ServiceDeployConfig, error) {
	if len(services) == 0 {
		return nil, nil
	}

	serviceByName := make(map[string]ServiceDeployConfig, len(services))
	inDegree := make(map[string]int, len(services))
	adj := make(map[string][]string, len(services))

	for _, svc := range services {
		name := strings.TrimSpace(svc.Spec.Name)
		if name == "" {
			return nil, fmt.Errorf("topological sort: service name is required")
		}
		if _, exists := serviceByName[name]; exists {
			return nil, fmt.Errorf("topological sort: duplicate service %q", name)
		}
		serviceByName[name] = svc
		inDegree[name] = 0
		adj[name] = nil
	}

	for _, svc := range services {
		name := svc.Spec.Name
		for _, dep := range svc.DependsOn {
			dep = strings.TrimSpace(dep)
			if dep == "" {
				continue
			}
			if dep == name {
				return nil, fmt.Errorf("topological sort: service %q depends on itself", name)
			}
			if _, ok := serviceByName[dep]; !ok {
				return nil, fmt.Errorf("topological sort: service %q depends on unknown service %q", name, dep)
			}
			adj[dep] = append(adj[dep], name)
			inDegree[name]++
		}
	}

	for name := range adj {
		sort.Strings(adj[name])
	}

	ready := make([]string, 0, len(services))
	for name, degree := range inDegree {
		if degree == 0 {
			ready = append(ready, name)
		}
	}
	sort.Strings(ready)

	processed := 0
	tiers := make([][]ServiceDeployConfig, 0)
	for len(ready) > 0 {
		currentTierNames := append([]string(nil), ready...)
		ready = ready[:0]

		tier := make([]ServiceDeployConfig, 0, len(currentTierNames))
		for _, name := range currentTierNames {
			tier = append(tier, serviceByName[name])
			processed++
			for _, dependent := range adj[name] {
				inDegree[dependent]--
				if inDegree[dependent] == 0 {
					ready = append(ready, dependent)
				}
			}
		}
		sort.Strings(ready)
		tiers = append(tiers, tier)
	}

	if processed != len(services) {
		remaining := make([]string, 0, len(services)-processed)
		for name, degree := range inDegree {
			if degree > 0 {
				remaining = append(remaining, name)
			}
		}
		sort.Strings(remaining)
		return nil, fmt.Errorf("topological sort: dependency cycle detected among services %v", remaining)
	}

	return tiers, nil
}

// DetectUpdateOrder determines whether a service update should use stop-first
// or start-first.
func DetectUpdateOrder(plan ServicePlan, incoming UpdateConfig) string {
	if hasPortConflicts(plan.NeedsRecreate) {
		return updateOrderStopFirst
	}
	if singleReplicaWithMounts(plan) {
		return updateOrderStopFirst
	}
	if order := strings.TrimSpace(incoming.Order); order != "" {
		return order
	}
	return updateOrderStartFirst
}

func classifyPlannedChange(current, incoming ServiceSpec, rawCurrentSpecJSON string) (ChangeKind, string) {
	kind := ClassifyChange(current, incoming)
	switch kind {
	case UpToDate:
		incomingJSON, err := json.Marshal(canonicalSpec(incoming))
		if err == nil && strings.TrimSpace(rawCurrentSpecJSON) != "" {
			if strings.TrimSpace(rawCurrentSpecJSON) != string(incomingJSON) {
				return NeedsSpecUpdate, "spec metadata changed"
			}
		}
		return UpToDate, "up-to-date"
	case NeedsUpdate:
		return NeedsUpdate, needsUpdateReason(current, incoming)
	case NeedsRecreate:
		return NeedsRecreate, needsRecreateReason(current, incoming)
	default:
		return kind, "spec changed"
	}
}

func needsUpdateReason(current, incoming ServiceSpec) string {
	if current.Resources == nil && incoming.Resources != nil {
		return "resources added"
	}
	if current.Resources != nil && incoming.Resources == nil {
		return "resources removed"
	}
	if current.Resources != nil && incoming.Resources != nil {
		if current.Resources.CPULimit != incoming.Resources.CPULimit && current.Resources.MemoryLimit != incoming.Resources.MemoryLimit {
			return fmt.Sprintf("resources changed: CPU %.3f→%.3f, memory %d→%d", current.Resources.CPULimit, incoming.Resources.CPULimit, current.Resources.MemoryLimit, incoming.Resources.MemoryLimit)
		}
		if current.Resources.CPULimit != incoming.Resources.CPULimit {
			return fmt.Sprintf("CPU limit changed: %.3f→%.3f", current.Resources.CPULimit, incoming.Resources.CPULimit)
		}
		if current.Resources.MemoryLimit != incoming.Resources.MemoryLimit {
			return fmt.Sprintf("memory limit changed: %d→%d", current.Resources.MemoryLimit, incoming.Resources.MemoryLimit)
		}
	}
	return "resources changed"
}

func needsRecreateReason(current, incoming ServiceSpec) string {
	if current.Image != incoming.Image {
		return fmt.Sprintf("image changed: %s → %s", current.Image, incoming.Image)
	}
	if !slicesEqual(current.Command, incoming.Command) {
		return "command changed"
	}
	if !slicesEqual(current.Entrypoint, incoming.Entrypoint) {
		return "entrypoint changed"
	}
	if !slicesEqual(current.Environment, incoming.Environment) {
		return "environment changed"
	}
	if !mountsEqual(current.Mounts, incoming.Mounts) {
		return "mounts changed"
	}
	if !portsEqual(current.Ports, incoming.Ports) {
		return "ports changed"
	}
	if !labelsEqual(current.Labels, incoming.Labels) {
		return "labels changed"
	}
	if current.RestartPolicy != incoming.RestartPolicy {
		return "restart policy changed"
	}
	if !healthChecksEqual(current.HealthCheck, incoming.HealthCheck) {
		return "health check changed"
	}
	return "service spec changed"
}

func createReason(service string, currentCountByService, targetCountByService map[string]int) string {
	current := currentCountByService[service]
	target := targetCountByService[service]
	if current == 0 {
		return "new service"
	}
	if target > current {
		return fmt.Sprintf("scaling %d → %d: adding %d replicas", current, target, target-current)
	}
	return "new assignment"
}

func removeReason(service string, currentCountByService, targetCountByService map[string]int) string {
	current := currentCountByService[service]
	target := targetCountByService[service]
	if target == 0 {
		return "service removed"
	}
	if target < current {
		return fmt.Sprintf("scaling %d → %d: removing %d replicas", current, target, current-target)
	}
	return "remove stale assignment"
}

func popCurrentRow(index map[string]map[string][]ContainerRow, service, machineID string) (ContainerRow, bool) {
	serviceRows := index[service]
	if serviceRows == nil {
		return ContainerRow{}, false
	}
	rows := serviceRows[machineID]
	if len(rows) == 0 {
		return ContainerRow{}, false
	}
	row := rows[0]
	serviceRows[machineID] = rows[1:]
	return row, true
}

func decodeServiceSpec(raw string) (ServiceSpec, error) {
	if strings.TrimSpace(raw) == "" {
		return ServiceSpec{}, fmt.Errorf("empty spec json")
	}
	var spec ServiceSpec
	if err := json.Unmarshal([]byte(raw), &spec); err != nil {
		return ServiceSpec{}, fmt.Errorf("decode service spec: %w", err)
	}
	return canonicalSpec(spec), nil
}

func cloneContainerRowPtr(row ContainerRow) *ContainerRow {
	out := row
	return &out
}

func deterministicDeployID(incoming DeploySpec, schedule map[string][]MachineAssignment) string {
	h := sha256.New()
	h.Write([]byte(strings.TrimSpace(incoming.Namespace)))
	h.Write([]byte{'\n'})

	serviceNames := make([]string, 0, len(incoming.Services))
	for _, svc := range incoming.Services {
		serviceNames = append(serviceNames, svc.Spec.Name)
	}
	sort.Strings(serviceNames)
	for _, name := range serviceNames {
		h.Write([]byte(name))
		h.Write([]byte{':'})
		assignments := append([]MachineAssignment(nil), schedule[name]...)
		sort.Slice(assignments, func(i, j int) bool {
			if assignments[i].MachineID != assignments[j].MachineID {
				return assignments[i].MachineID < assignments[j].MachineID
			}
			return assignments[i].ContainerName < assignments[j].ContainerName
		})
		for _, assignment := range assignments {
			h.Write([]byte(assignment.MachineID))
			h.Write([]byte{'|'})
			h.Write([]byte(assignment.ContainerName))
			h.Write([]byte{';'})
		}
		h.Write([]byte{'\n'})
	}
	sum := h.Sum(nil)
	return hex.EncodeToString(sum[:8])
}

func hasPortConflicts(entries []PlanEntry) bool {
	for _, entry := range entries {
		if entry.CurrentRow == nil {
			continue
		}
		currentSpec, err := decodeServiceSpec(entry.CurrentRow.SpecJSON)
		if err != nil {
			return true
		}
		for _, oldPort := range currentSpec.Ports {
			for _, newPort := range entry.Spec.Ports {
				if oldPort.HostPort == 0 || newPort.HostPort == 0 {
					continue
				}
				if oldPort.HostPort == newPort.HostPort && strings.EqualFold(oldPort.Protocol, newPort.Protocol) {
					return true
				}
			}
		}
	}
	return false
}

func singleReplicaWithMounts(plan ServicePlan) bool {
	replicas := len(plan.UpToDate) + len(plan.NeedsSpecUpdate) + len(plan.NeedsUpdate) + len(plan.NeedsRecreate) + len(plan.Create)
	if replicas != 1 {
		return false
	}
	for _, entry := range plan.NeedsRecreate {
		if len(entry.Spec.Mounts) > 0 {
			return true
		}
	}
	for _, entry := range plan.Create {
		if len(entry.Spec.Mounts) > 0 {
			return true
		}
	}
	return false
}

func slicesEqual(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func mountsEqual(a, b []Mount) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func portsEqual(a, b []PortMapping) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func labelsEqual(a, b map[string]string) bool {
	if len(a) != len(b) {
		return false
	}
	for key, value := range a {
		if b[key] != value {
			return false
		}
	}
	return true
}

func healthChecksEqual(a, b *HealthCheck) bool {
	if a == nil && b == nil {
		return true
	}
	if a == nil || b == nil {
		return false
	}
	if !slicesEqual(a.Test, b.Test) {
		return false
	}
	return a.Interval == b.Interval && a.Timeout == b.Timeout && a.Retries == b.Retries && a.StartPeriod == b.StartPeriod
}
