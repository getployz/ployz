package deploy

import (
	"fmt"
	"path/filepath"
	"sort"
	"strings"
)

const (
	constraintLabelPrefix = "node.labels."
	defaultReplicaCount   = 1
)

// Schedule assigns services to machines based on placement mode, constraints,
// volume affinity, and replica count.
func Schedule(
	namespace string,
	services []ServiceDeployConfig,
	machines []MachineInfo,
	currentContainers []ContainerRow,
) (map[string][]MachineAssignment, error) {
	if len(services) == 0 {
		return map[string][]MachineAssignment{}, nil
	}
	if len(machines) == 0 {
		return nil, fmt.Errorf("schedule: no machines available")
	}

	machineByID := make(map[string]MachineInfo, len(machines))
	machineIDs := make([]string, 0, len(machines))
	for _, machine := range machines {
		if strings.TrimSpace(machine.ID) == "" {
			continue
		}
		machineByID[machine.ID] = machine
		machineIDs = append(machineIDs, machine.ID)
	}
	if len(machineIDs) == 0 {
		return nil, fmt.Errorf("schedule: no machines with valid IDs")
	}
	sort.Strings(machineIDs)

	eligibleByService := make(map[string][]string, len(services))
	serviceByName := make(map[string]ServiceDeployConfig, len(services))
	for _, svc := range services {
		name := strings.TrimSpace(svc.Spec.Name)
		if name == "" {
			return nil, fmt.Errorf("schedule: service name is required")
		}
		if _, exists := serviceByName[name]; exists {
			return nil, fmt.Errorf("schedule: duplicate service name %q", name)
		}
		serviceByName[name] = svc

		eligible := make([]string, 0, len(machineIDs))
		for _, machineID := range machineIDs {
			if MatchConstraints(svc.Constraints, machineByID[machineID]) {
				eligible = append(eligible, machineID)
			}
		}
		if len(eligible) == 0 {
			return nil, fmt.Errorf("schedule service %q: no eligible machines for constraints %v", name, svc.Constraints)
		}
		eligibleByService[name] = eligible
	}

	groups := buildVolumeAffinityGroups(services)
	for _, group := range groups {
		if len(group) <= 1 {
			continue
		}
		common := intersectEligibleSets(group, eligibleByService)
		if len(common) == 0 {
			return nil, fmt.Errorf("schedule volume-affinity group %v: no shared eligible machines", group)
		}
		for _, serviceName := range group {
			eligibleByService[serviceName] = common
		}
		if err := validateAffinityReplicaCounts(group, serviceByName, len(common)); err != nil {
			return nil, err
		}
	}

	existingCounts := countExistingByServiceAndMachine(currentContainers)
	assignments := make(map[string][]MachineAssignment, len(services))

	for _, group := range groups {
		if len(group) == 0 {
			continue
		}

		if len(group) == 1 {
			name := group[0]
			svc := serviceByName[name]
			eligible := eligibleByService[name]
			assignmentIDs, err := assignServiceMachines(svc, eligible, existingCounts[name])
			if err != nil {
				return nil, err
			}
			assignments[name] = machineIDsToAssignments(namespace, name, assignmentIDs)
			continue
		}

		referenceName := group[0]
		referenceSvc := serviceByName[referenceName]
		sharedEligible := eligibleByService[referenceName]
		referenceCount := desiredReplicaCount(referenceSvc)
		if referenceSvc.Placement == PlacementGlobal {
			referenceCount = len(sharedEligible)
		}

		groupCounts := make(map[string]int, len(sharedEligible))
		for _, serviceName := range group {
			for machineID, count := range existingCounts[serviceName] {
				groupCounts[machineID] += count
			}
		}

		groupMachines := chooseReplicatedMachines(referenceCount, sharedEligible, groupCounts)
		for _, serviceName := range group {
			svc := serviceByName[serviceName]
			if svc.Placement == PlacementGlobal {
				assignments[serviceName] = machineIDsToAssignments(namespace, serviceName, sharedEligible)
				continue
			}
			assignments[serviceName] = machineIDsToAssignments(namespace, serviceName, groupMachines)
		}
	}

	for _, svc := range services {
		name := svc.Spec.Name
		if _, ok := assignments[name]; !ok {
			assignments[name] = nil
		}
	}

	return assignments, nil
}

// MatchConstraints evaluates all placement constraints against a machine's labels.
func MatchConstraints(constraints []string, machine MachineInfo) bool {
	for _, constraint := range constraints {
		key, op, value, err := ParseConstraint(constraint)
		if err != nil {
			return false
		}
		machineValue, exists := machine.Labels[key]
		switch op {
		case "==":
			if !exists || machineValue != value {
				return false
			}
		case "!=":
			if exists && machineValue == value {
				return false
			}
		default:
			return false
		}
	}
	return true
}

// ParseConstraint extracts key/operator/value from a placement constraint.
func ParseConstraint(s string) (key, op, value string, err error) {
	s = strings.TrimSpace(s)
	if s == "" {
		return "", "", "", fmt.Errorf("empty constraint")
	}

	if strings.Contains(s, "==") {
		op = "=="
	} else if strings.Contains(s, "!=") {
		op = "!="
	} else {
		return "", "", "", fmt.Errorf("unsupported constraint operator: %q", s)
	}

	parts := strings.SplitN(s, op, 2)
	if len(parts) != 2 {
		return "", "", "", fmt.Errorf("invalid constraint format: %q", s)
	}

	rawKey := strings.TrimSpace(parts[0])
	value = strings.TrimSpace(parts[1])
	if value == "" {
		return "", "", "", fmt.Errorf("constraint value is required: %q", s)
	}
	if !strings.HasPrefix(rawKey, constraintLabelPrefix) {
		return "", "", "", fmt.Errorf("unsupported constraint key: %q", rawKey)
	}
	key = strings.TrimSpace(strings.TrimPrefix(rawKey, constraintLabelPrefix))
	if key == "" {
		return "", "", "", fmt.Errorf("constraint key is required: %q", s)
	}

	return key, op, value, nil
}

func countExistingByServiceAndMachine(rows []ContainerRow) map[string]map[string]int {
	out := make(map[string]map[string]int)
	for _, row := range rows {
		if strings.TrimSpace(row.Service) == "" || strings.TrimSpace(row.MachineID) == "" {
			continue
		}
		if out[row.Service] == nil {
			out[row.Service] = make(map[string]int)
		}
		out[row.Service][row.MachineID]++
	}
	return out
}

func desiredReplicaCount(svc ServiceDeployConfig) int {
	if svc.Placement == PlacementGlobal {
		return 0
	}
	if svc.Replicas <= 0 {
		return defaultReplicaCount
	}
	return svc.Replicas
}

func assignServiceMachines(svc ServiceDeployConfig, eligible []string, existing map[string]int) ([]string, error) {
	if len(eligible) == 0 {
		return nil, fmt.Errorf("schedule service %q: no eligible machines", svc.Spec.Name)
	}
	if svc.Placement == PlacementGlobal {
		out := make([]string, len(eligible))
		copy(out, eligible)
		return out, nil
	}
	replicas := desiredReplicaCount(svc)
	return chooseReplicatedMachines(replicas, eligible, existing), nil
}

func chooseReplicatedMachines(replicas int, eligible []string, existing map[string]int) []string {
	if replicas <= 0 || len(eligible) == 0 {
		return nil
	}

	out := make([]string, 0, replicas)
	newCounts := make(map[string]int, len(eligible))
	cursor := 0

	for range replicas {
		bestIdx := -1
		bestScore := int(^uint(0) >> 1)

		for offset := range eligible {
			idx := (cursor + offset) % len(eligible)
			machineID := eligible[idx]
			score := existing[machineID] + newCounts[machineID]
			if score < bestScore {
				bestScore = score
				bestIdx = idx
			}
		}

		if bestIdx < 0 {
			break
		}
		chosen := eligible[bestIdx]
		out = append(out, chosen)
		newCounts[chosen]++
		cursor = (bestIdx + 1) % len(eligible)
	}

	return out
}

func machineIDsToAssignments(namespace, service string, machineIDs []string) []MachineAssignment {
	out := make([]MachineAssignment, 0, len(machineIDs))
	for _, machineID := range machineIDs {
		out = append(out, MachineAssignment{
			MachineID:     machineID,
			ContainerName: ContainerName(namespace, service),
		})
	}
	return out
}

func buildVolumeAffinityGroups(services []ServiceDeployConfig) [][]string {
	serviceNames := make([]string, 0, len(services))
	for _, svc := range services {
		serviceNames = append(serviceNames, svc.Spec.Name)
	}
	sort.Strings(serviceNames)

	parent := make(map[string]string, len(serviceNames))
	for _, name := range serviceNames {
		parent[name] = name
	}

	var find func(string) string
	find = func(x string) string {
		if parent[x] == x {
			return x
		}
		parent[x] = find(parent[x])
		return parent[x]
	}

	union := func(a, b string) {
		ra := find(a)
		rb := find(b)
		if ra == rb {
			return
		}
		if ra < rb {
			parent[rb] = ra
			return
		}
		parent[ra] = rb
	}

	volumeToServices := make(map[string][]string)
	for _, svc := range services {
		for _, mount := range svc.Spec.Mounts {
			if !isNamedVolumeSource(mount.Source) {
				continue
			}
			volumeToServices[mount.Source] = append(volumeToServices[mount.Source], svc.Spec.Name)
		}
	}

	for _, names := range volumeToServices {
		if len(names) < 2 {
			continue
		}
		sort.Strings(names)
		anchor := names[0]
		for _, name := range names[1:] {
			union(anchor, name)
		}
	}

	groupMap := make(map[string][]string)
	for _, name := range serviceNames {
		root := find(name)
		groupMap[root] = append(groupMap[root], name)
	}

	roots := make([]string, 0, len(groupMap))
	for root := range groupMap {
		roots = append(roots, root)
	}
	sort.Strings(roots)

	out := make([][]string, 0, len(groupMap))
	for _, root := range roots {
		group := groupMap[root]
		sort.Strings(group)
		out = append(out, group)
	}
	return out
}

func intersectEligibleSets(group []string, eligibleByService map[string][]string) []string {
	if len(group) == 0 {
		return nil
	}
	set := make(map[string]int)
	for idx, serviceName := range group {
		for _, machineID := range eligibleByService[serviceName] {
			if idx == 0 {
				set[machineID] = 1
				continue
			}
			if set[machineID] == idx {
				set[machineID] = idx + 1
			}
		}
	}
	out := make([]string, 0, len(set))
	for machineID, count := range set {
		if count == len(group) {
			out = append(out, machineID)
		}
	}
	sort.Strings(out)
	return out
}

func validateAffinityReplicaCounts(group []string, serviceByName map[string]ServiceDeployConfig, eligibleCount int) error {
	if len(group) <= 1 {
		return nil
	}
	anchor := group[0]
	anchorSvc := serviceByName[anchor]
	anchorReplicas := desiredReplicaCount(anchorSvc)
	if anchorSvc.Placement == PlacementGlobal {
		anchorReplicas = eligibleCount
	}

	for _, serviceName := range group[1:] {
		svc := serviceByName[serviceName]
		replicas := desiredReplicaCount(svc)
		if svc.Placement == PlacementGlobal {
			replicas = eligibleCount
		}
		if replicas != anchorReplicas {
			return fmt.Errorf("schedule volume-affinity group %v: replica counts must match (%s=%d, %s=%d)", group, anchor, anchorReplicas, serviceName, replicas)
		}
	}
	return nil
}

func isNamedVolumeSource(source string) bool {
	source = strings.TrimSpace(source)
	if source == "" {
		return false
	}
	if filepath.IsAbs(source) {
		return false
	}
	if strings.HasPrefix(source, "./") || strings.HasPrefix(source, "../") || strings.HasPrefix(source, "~") {
		return false
	}
	if strings.Contains(source, `\\`) || strings.Contains(source, "/") {
		return false
	}
	return true
}
