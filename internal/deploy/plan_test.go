package deploy

import (
	"encoding/json"
	"fmt"
	"strings"
	"testing"
)

func TestTopologicalSort(t *testing.T) {
	t.Run("no dependencies", func(t *testing.T) {
		services := []ServiceDeployConfig{
			{Spec: ServiceSpec{Name: "b"}},
			{Spec: ServiceSpec{Name: "a"}},
		}

		tiers, err := TopologicalSort(services)
		if err != nil {
			t.Fatalf("TopologicalSort() error = %v", err)
		}
		if len(tiers) != 1 {
			t.Fatalf("tier count = %d, want 1", len(tiers))
		}
		if tiers[0][0].Spec.Name != "a" || tiers[0][1].Spec.Name != "b" {
			t.Fatalf("tier[0] names = [%s, %s], want [a, b]", tiers[0][0].Spec.Name, tiers[0][1].Spec.Name)
		}
	})

	t.Run("linear chain", func(t *testing.T) {
		services := []ServiceDeployConfig{
			{Spec: ServiceSpec{Name: "a"}},
			{Spec: ServiceSpec{Name: "b"}, DependsOn: []string{"a"}},
			{Spec: ServiceSpec{Name: "c"}, DependsOn: []string{"b"}},
		}

		tiers, err := TopologicalSort(services)
		if err != nil {
			t.Fatalf("TopologicalSort() error = %v", err)
		}
		if len(tiers) != 3 {
			t.Fatalf("tier count = %d, want 3", len(tiers))
		}
		if tiers[0][0].Spec.Name != "a" || tiers[1][0].Spec.Name != "b" || tiers[2][0].Spec.Name != "c" {
			t.Fatalf("unexpected tier ordering: %+v", tiers)
		}
	})

	t.Run("diamond graph", func(t *testing.T) {
		services := []ServiceDeployConfig{
			{Spec: ServiceSpec{Name: "a"}},
			{Spec: ServiceSpec{Name: "b"}, DependsOn: []string{"a"}},
			{Spec: ServiceSpec{Name: "c"}, DependsOn: []string{"a"}},
			{Spec: ServiceSpec{Name: "d"}, DependsOn: []string{"b", "c"}},
		}

		tiers, err := TopologicalSort(services)
		if err != nil {
			t.Fatalf("TopologicalSort() error = %v", err)
		}
		if len(tiers) != 3 {
			t.Fatalf("tier count = %d, want 3", len(tiers))
		}
		if tiers[0][0].Spec.Name != "a" {
			t.Fatalf("tier 0 = %q, want a", tiers[0][0].Spec.Name)
		}
		if len(tiers[1]) != 2 || tiers[1][0].Spec.Name != "b" || tiers[1][1].Spec.Name != "c" {
			t.Fatalf("tier 1 names = [%s, %s], want [b, c]", tiers[1][0].Spec.Name, tiers[1][1].Spec.Name)
		}
		if tiers[2][0].Spec.Name != "d" {
			t.Fatalf("tier 2 = %q, want d", tiers[2][0].Spec.Name)
		}
	})

	t.Run("cycle", func(t *testing.T) {
		services := []ServiceDeployConfig{
			{Spec: ServiceSpec{Name: "a"}, DependsOn: []string{"b"}},
			{Spec: ServiceSpec{Name: "b"}, DependsOn: []string{"a"}},
		}
		_, err := TopologicalSort(services)
		if err == nil {
			t.Fatal("TopologicalSort() expected cycle error")
		}
	})

	t.Run("self dependency", func(t *testing.T) {
		services := []ServiceDeployConfig{{Spec: ServiceSpec{Name: "a"}, DependsOn: []string{"a"}}}
		_, err := TopologicalSort(services)
		if err == nil {
			t.Fatal("TopologicalSort() expected self dependency error")
		}
	})
}

func TestPlanDeploy(t *testing.T) {
	t.Run("fresh deploy", func(t *testing.T) {
		incoming := DeploySpec{
			Namespace: "ns",
			Services: []ServiceDeployConfig{
				{Spec: ServiceSpec{Name: "db", Image: "postgres:16"}},
				{Spec: ServiceSpec{Name: "app", Image: "ghcr.io/example/app:latest"}, DependsOn: []string{"db"}},
			},
		}
		schedule := map[string][]MachineAssignment{
			"db":  {{MachineID: "a", ContainerName: "ployz-ns-db-1111"}},
			"app": {{MachineID: "b", ContainerName: "ployz-ns-app-2222"}},
		}

		plan, err := PlanDeploy(incoming, nil, schedule)
		if err != nil {
			t.Fatalf("PlanDeploy() error = %v", err)
		}
		if plan.Namespace != "ns" {
			t.Fatalf("plan.Namespace = %q, want ns", plan.Namespace)
		}
		if plan.DeployID == "" {
			t.Fatal("plan.DeployID should not be empty")
		}
		if len(plan.Tiers) != 2 {
			t.Fatalf("tier count = %d, want 2", len(plan.Tiers))
		}

		db := mustFindServicePlan(t, plan, "db")
		if len(db.Create) != 1 || db.Create[0].ContainerName != "ployz-ns-db-1111" {
			t.Fatalf("db create = %+v, want one create with scheduled name", db.Create)
		}

		app := mustFindServicePlan(t, plan, "app")
		if len(app.Create) != 1 || app.Create[0].ContainerName != "ployz-ns-app-2222" {
			t.Fatalf("app create = %+v, want one create with scheduled name", app.Create)
		}
	})

	t.Run("no changes", func(t *testing.T) {
		spec := ServiceSpec{Name: "web", Image: "nginx:1.25"}
		current := []ContainerRow{fixtureRow("row-web-1", "ns", "web", "a", "ployz-ns-web-old", spec)}
		incoming := DeploySpec{Namespace: "ns", Services: []ServiceDeployConfig{{Spec: spec}}}
		schedule := map[string][]MachineAssignment{"web": {{MachineID: "a", ContainerName: "ployz-ns-web-new"}}}

		plan, err := PlanDeploy(incoming, current, schedule)
		if err != nil {
			t.Fatalf("PlanDeploy() error = %v", err)
		}
		web := mustFindServicePlan(t, plan, "web")
		if len(web.UpToDate) != 1 {
			t.Fatalf("UpToDate len = %d, want 1", len(web.UpToDate))
		}
		if web.UpToDate[0].ContainerName != "ployz-ns-web-old" {
			t.Fatalf("UpToDate container name = %q, want existing name", web.UpToDate[0].ContainerName)
		}
		if len(web.Create)+len(web.NeedsUpdate)+len(web.NeedsRecreate)+len(web.Remove) != 0 {
			t.Fatalf("expected no changes, got create=%d update=%d recreate=%d remove=%d", len(web.Create), len(web.NeedsUpdate), len(web.NeedsRecreate), len(web.Remove))
		}
	})

	t.Run("image change needs recreate", func(t *testing.T) {
		currentSpec := ServiceSpec{Name: "web", Image: "nginx:1.25"}
		incomingSpec := ServiceSpec{Name: "web", Image: "nginx:1.26"}
		current := []ContainerRow{fixtureRow("row-web-1", "ns", "web", "a", "ployz-ns-web-old", currentSpec)}
		incoming := DeploySpec{Namespace: "ns", Services: []ServiceDeployConfig{{Spec: incomingSpec}}}
		schedule := map[string][]MachineAssignment{"web": {{MachineID: "a", ContainerName: "ployz-ns-web-new"}}}

		plan, err := PlanDeploy(incoming, current, schedule)
		if err != nil {
			t.Fatalf("PlanDeploy() error = %v", err)
		}
		web := mustFindServicePlan(t, plan, "web")
		if len(web.NeedsRecreate) != 1 {
			t.Fatalf("NeedsRecreate len = %d, want 1", len(web.NeedsRecreate))
		}
		entry := web.NeedsRecreate[0]
		if entry.ContainerName != "ployz-ns-web-new" {
			t.Fatalf("new container name = %q, want scheduled name", entry.ContainerName)
		}
		if entry.CurrentRow == nil || entry.CurrentRow.ContainerName != "ployz-ns-web-old" {
			t.Fatalf("CurrentRow = %+v, want existing row", entry.CurrentRow)
		}
		if !strings.Contains(entry.Reason, "image changed") {
			t.Fatalf("reason = %q, want image-changed context", entry.Reason)
		}
	})

	t.Run("resource-only change needs update", func(t *testing.T) {
		currentSpec := ServiceSpec{Name: "web", Image: "nginx:1.25", Resources: &Resources{CPULimit: 1, MemoryLimit: 128}}
		incomingSpec := ServiceSpec{Name: "web", Image: "nginx:1.25", Resources: &Resources{CPULimit: 2, MemoryLimit: 128}}
		current := []ContainerRow{fixtureRow("row-web-1", "ns", "web", "a", "ployz-ns-web-old", currentSpec)}
		incoming := DeploySpec{Namespace: "ns", Services: []ServiceDeployConfig{{Spec: incomingSpec}}}
		schedule := map[string][]MachineAssignment{"web": {{MachineID: "a", ContainerName: "ployz-ns-web-new"}}}

		plan, err := PlanDeploy(incoming, current, schedule)
		if err != nil {
			t.Fatalf("PlanDeploy() error = %v", err)
		}
		web := mustFindServicePlan(t, plan, "web")
		if len(web.NeedsUpdate) != 1 {
			t.Fatalf("NeedsUpdate len = %d, want 1", len(web.NeedsUpdate))
		}
		if web.NeedsUpdate[0].ContainerName != "ployz-ns-web-old" {
			t.Fatalf("update container name = %q, want existing name", web.NeedsUpdate[0].ContainerName)
		}
	})

	t.Run("scale up", func(t *testing.T) {
		spec := ServiceSpec{Name: "api", Image: "ghcr.io/example/api:latest"}
		current := []ContainerRow{fixtureRow("row-api-1", "ns", "api", "a", "ployz-ns-api-a", spec)}
		incoming := DeploySpec{Namespace: "ns", Services: []ServiceDeployConfig{{Spec: spec}}}
		schedule := map[string][]MachineAssignment{
			"api": {
				{MachineID: "a", ContainerName: "ployz-ns-api-new-a"},
				{MachineID: "b", ContainerName: "ployz-ns-api-new-b"},
				{MachineID: "c", ContainerName: "ployz-ns-api-new-c"},
			},
		}

		plan, err := PlanDeploy(incoming, current, schedule)
		if err != nil {
			t.Fatalf("PlanDeploy() error = %v", err)
		}
		api := mustFindServicePlan(t, plan, "api")
		if len(api.UpToDate) != 1 || len(api.Create) != 2 {
			t.Fatalf("scale up classification mismatch: up_to_date=%d create=%d", len(api.UpToDate), len(api.Create))
		}
	})

	t.Run("scale down", func(t *testing.T) {
		spec := ServiceSpec{Name: "api", Image: "ghcr.io/example/api:latest"}
		current := []ContainerRow{
			fixtureRow("row-api-1", "ns", "api", "a", "ployz-ns-api-a", spec),
			fixtureRow("row-api-2", "ns", "api", "b", "ployz-ns-api-b", spec),
			fixtureRow("row-api-3", "ns", "api", "c", "ployz-ns-api-c", spec),
		}
		incoming := DeploySpec{Namespace: "ns", Services: []ServiceDeployConfig{{Spec: spec}}}
		schedule := map[string][]MachineAssignment{"api": {{MachineID: "a", ContainerName: "ployz-ns-api-new-a"}}}

		plan, err := PlanDeploy(incoming, current, schedule)
		if err != nil {
			t.Fatalf("PlanDeploy() error = %v", err)
		}
		api := mustFindServicePlan(t, plan, "api")
		if len(api.UpToDate) != 1 || len(api.Remove) != 2 {
			t.Fatalf("scale down classification mismatch: up_to_date=%d remove=%d", len(api.UpToDate), len(api.Remove))
		}
	})

	t.Run("service added and removed", func(t *testing.T) {
		oldSpec := ServiceSpec{Name: "old", Image: "busybox:latest"}
		newSpec := ServiceSpec{Name: "new", Image: "nginx:1.25"}
		current := []ContainerRow{fixtureRow("row-old-1", "ns", "old", "a", "ployz-ns-old-a", oldSpec)}
		incoming := DeploySpec{Namespace: "ns", Services: []ServiceDeployConfig{{Spec: newSpec}}}
		schedule := map[string][]MachineAssignment{"new": {{MachineID: "a", ContainerName: "ployz-ns-new-a"}}}

		plan, err := PlanDeploy(incoming, current, schedule)
		if err != nil {
			t.Fatalf("PlanDeploy() error = %v", err)
		}
		newPlan := mustFindServicePlan(t, plan, "new")
		if len(newPlan.Create) != 1 {
			t.Fatalf("new service create len = %d, want 1", len(newPlan.Create))
		}
		oldPlan := mustFindServicePlan(t, plan, "old")
		if len(oldPlan.Remove) != 1 || oldPlan.Remove[0].Reason != "service removed" {
			t.Fatalf("old service remove = %+v, want one remove with reason service removed", oldPlan.Remove)
		}
	})

	t.Run("mixed changes", func(t *testing.T) {
		aSpec := ServiceSpec{Name: "a", Image: "nginx:1.25"}
		bCurrent := ServiceSpec{Name: "b", Image: "redis:7"}
		bIncoming := ServiceSpec{Name: "b", Image: "redis:8"}
		cSpec := ServiceSpec{Name: "c", Image: "busybox:latest"}
		dSpec := ServiceSpec{Name: "d", Image: "postgres:16"}

		current := []ContainerRow{
			fixtureRow("row-a", "ns", "a", "a", "ployz-ns-a-old", aSpec),
			fixtureRow("row-b", "ns", "b", "b", "ployz-ns-b-old", bCurrent),
			fixtureRow("row-d", "ns", "d", "c", "ployz-ns-d-old", dSpec),
		}
		incoming := DeploySpec{Namespace: "ns", Services: []ServiceDeployConfig{{Spec: aSpec}, {Spec: bIncoming}, {Spec: cSpec}}}
		schedule := map[string][]MachineAssignment{
			"a": {{MachineID: "a", ContainerName: "ployz-ns-a-new"}},
			"b": {{MachineID: "b", ContainerName: "ployz-ns-b-new"}},
			"c": {{MachineID: "c", ContainerName: "ployz-ns-c-new"}},
		}

		plan, err := PlanDeploy(incoming, current, schedule)
		if err != nil {
			t.Fatalf("PlanDeploy() error = %v", err)
		}
		aPlan := mustFindServicePlan(t, plan, "a")
		if len(aPlan.UpToDate) != 1 {
			t.Fatalf("service a UpToDate len = %d, want 1", len(aPlan.UpToDate))
		}
		bPlan := mustFindServicePlan(t, plan, "b")
		if len(bPlan.NeedsRecreate) != 1 {
			t.Fatalf("service b NeedsRecreate len = %d, want 1", len(bPlan.NeedsRecreate))
		}
		cPlan := mustFindServicePlan(t, plan, "c")
		if len(cPlan.Create) != 1 {
			t.Fatalf("service c Create len = %d, want 1", len(cPlan.Create))
		}
		dPlan := mustFindServicePlan(t, plan, "d")
		if len(dPlan.Remove) != 1 {
			t.Fatalf("service d Remove len = %d, want 1", len(dPlan.Remove))
		}
	})
}

func TestDetectUpdateOrder(t *testing.T) {
	t.Run("overlapping host ports forces stop-first", func(t *testing.T) {
		current := ServiceSpec{Name: "web", Image: "nginx:1.25", Ports: []PortMapping{{HostPort: 8080, ContainerPort: 80, Protocol: "tcp"}}}
		incoming := ServiceSpec{Name: "web", Image: "nginx:1.26", Ports: []PortMapping{{HostPort: 8080, ContainerPort: 80, Protocol: "tcp"}}}
		plan := ServicePlan{NeedsRecreate: []PlanEntry{fixtureRecreateEntry("a", current, incoming)}}

		order := DetectUpdateOrder(plan, UpdateConfig{Order: updateOrderStartFirst})
		if order != updateOrderStopFirst {
			t.Fatalf("order = %q, want %q", order, updateOrderStopFirst)
		}
	})

	t.Run("non-overlapping ports respects config", func(t *testing.T) {
		current := ServiceSpec{Name: "web", Image: "nginx:1.25", Ports: []PortMapping{{HostPort: 8080, ContainerPort: 80, Protocol: "tcp"}}}
		incoming := ServiceSpec{Name: "web", Image: "nginx:1.26", Ports: []PortMapping{{HostPort: 9090, ContainerPort: 80, Protocol: "tcp"}}}
		plan := ServicePlan{NeedsRecreate: []PlanEntry{fixtureRecreateEntry("a", current, incoming)}}

		order := DetectUpdateOrder(plan, UpdateConfig{Order: updateOrderStartFirst})
		if order != updateOrderStartFirst {
			t.Fatalf("order = %q, want %q", order, updateOrderStartFirst)
		}
	})

	t.Run("single replica with mounts forces stop-first", func(t *testing.T) {
		current := ServiceSpec{Name: "web", Image: "nginx:1.25", Mounts: []Mount{{Source: "data", Target: "/data"}}}
		incoming := ServiceSpec{Name: "web", Image: "nginx:1.26", Mounts: []Mount{{Source: "data", Target: "/data"}}}
		plan := ServicePlan{NeedsRecreate: []PlanEntry{fixtureRecreateEntry("a", current, incoming)}}

		order := DetectUpdateOrder(plan, UpdateConfig{Order: updateOrderStartFirst})
		if order != updateOrderStopFirst {
			t.Fatalf("order = %q, want %q", order, updateOrderStopFirst)
		}
	})

	t.Run("multiple replicas with mounts respects config", func(t *testing.T) {
		current := ServiceSpec{Name: "web", Image: "nginx:1.25", Mounts: []Mount{{Source: "data", Target: "/data"}}}
		incoming := ServiceSpec{Name: "web", Image: "nginx:1.26", Mounts: []Mount{{Source: "data", Target: "/data"}}}
		plan := ServicePlan{
			NeedsRecreate: []PlanEntry{
				fixtureRecreateEntry("a", current, incoming),
				fixtureRecreateEntry("b", current, incoming),
			},
		}

		order := DetectUpdateOrder(plan, UpdateConfig{Order: updateOrderStartFirst})
		if order != updateOrderStartFirst {
			t.Fatalf("order = %q, want %q", order, updateOrderStartFirst)
		}
	})

	t.Run("default order is start-first", func(t *testing.T) {
		plan := ServicePlan{}
		order := DetectUpdateOrder(plan, UpdateConfig{})
		if order != updateOrderStartFirst {
			t.Fatalf("order = %q, want %q", order, updateOrderStartFirst)
		}
	})
}

func fixtureRow(id, namespace, service, machineID, containerName string, spec ServiceSpec) ContainerRow {
	return ContainerRow{
		ID:            id,
		Namespace:     namespace,
		Service:       service,
		MachineID:     machineID,
		ContainerName: containerName,
		SpecJSON:      mustServiceSpecJSON(spec),
	}
}

func fixtureRecreateEntry(machineID string, currentSpec, incomingSpec ServiceSpec) PlanEntry {
	row := fixtureRow(fmt.Sprintf("row-%s", machineID), "ns", incomingSpec.Name, machineID, "old-"+machineID, currentSpec)
	return PlanEntry{
		MachineID:     machineID,
		ContainerName: "new-" + machineID,
		Spec:          incomingSpec,
		CurrentRow:    &row,
	}
}

func mustServiceSpecJSON(spec ServiceSpec) string {
	data, err := json.Marshal(canonicalSpec(spec))
	if err != nil {
		panic(err)
	}
	return string(data)
}

func mustFindServicePlan(t *testing.T, plan DeployPlan, serviceName string) ServicePlan {
	t.Helper()
	for _, tier := range plan.Tiers {
		for _, service := range tier.Services {
			if service.Name == serviceName {
				return service
			}
		}
	}
	t.Fatalf("service %q not found in plan", serviceName)
	return ServicePlan{}
}
