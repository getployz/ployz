package deploy_test

import (
	"context"
	"encoding/json"
	"errors"
	"sync"
	"testing"
	"time"

	fakecluster "ployz/internal/adapter/fake/cluster"
	fakeleaf "ployz/internal/adapter/fake/leaf"
	"ployz/internal/deploy"
	"ployz/internal/mesh"
)

type chaosHarness struct {
	t       *testing.T
	clock   *fakeleaf.Clock
	cluster *fakecluster.Cluster
	nodes   map[string]*chaosNode
}

type chaosNode struct {
	runtime    *fakeleaf.ContainerRuntime
	containers *fakecluster.ClusterContainerStore
	deploys    *fakecluster.ClusterDeploymentStore
	health     *fakeleaf.HealthChecker
	reader     *runtimeStateReader
}

type runtimeStateReader struct {
	runtime *fakeleaf.ContainerRuntime
}

func (r *runtimeStateReader) ReadMachineState(ctx context.Context, machineID, namespace string) ([]deploy.ContainerState, error) {
	entries, err := r.runtime.ContainerList(ctx, map[string]string{"ployz.namespace": namespace})
	if err != nil {
		return nil, err
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

func newChaosHarness(t *testing.T, nodeIDs ...string) *chaosHarness {
	t.Helper()
	clock := fakeleaf.NewClock(time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC))
	cluster := fakecluster.NewCluster(clock)
	nodes := make(map[string]*chaosNode, len(nodeIDs))

	for _, nodeID := range nodeIDs {
		runtime := fakeleaf.NewContainerRuntime()
		node := &chaosNode{
			runtime:    runtime,
			containers: fakecluster.NewClusterContainerStore(cluster, nodeID),
			deploys:    fakecluster.NewClusterDeploymentStore(cluster, nodeID),
			health:     fakeleaf.NewHealthChecker(),
		}
		node.reader = &runtimeStateReader{runtime: runtime}
		nodes[nodeID] = node
	}

	return &chaosHarness{t: t, clock: clock, cluster: cluster, nodes: nodes}
}

func (h *chaosHarness) node(id string) *chaosNode {
	h.t.Helper()
	node := h.nodes[id]
	if node == nil {
		h.t.Fatalf("missing chaos node %q", id)
	}
	return node
}

func TestChaos_ContainerCrashAfterStart(t *testing.T) {
	h := newChaosHarness(t, "A", "B")
	nodeA := h.node("A")

	plan := deploy.DeployPlan{
		Namespace: "frontend",
		DeployID:  "deploy-crash",
		Tiers: []deploy.Tier{{
			Services: []deploy.ServicePlan{{
				Name: "app",
				HealthCheck: &deploy.HealthCheck{
					Test: []string{"CMD", "true"},
				},
				Create: []deploy.PlanEntry{{
					MachineID:     "A",
					ContainerName: "ployz-frontend-app-a001",
					Spec: deploy.ServiceSpec{
						Name:  "app",
						Image: "ghcr.io/example/app:v1",
					},
				}},
			}},
		}},
	}

	nodeA.health.SetUnhealthy("ployz-frontend-app-a001", errors.New("container crashed"))

	_, err := deploy.ApplyPlan(
		context.Background(),
		nodeA.runtime,
		deploy.Stores{Containers: nodeA.containers, Deployments: nodeA.deploys},
		nodeA.health,
		nodeA.reader,
		plan,
		"A",
		h.clock,
		nil,
	)
	if err == nil {
		t.Fatal("ApplyPlan() expected health failure")
	}
	assertDeployErrorPhase(t, err, "health")
	mustNotExist(t, nodeA.runtime, "ployz-frontend-app-a001")

	deploymentRow, ok, getErr := nodeA.deploys.GetDeployment(context.Background(), "deploy-crash")
	if getErr != nil || !ok {
		t.Fatalf("GetDeployment() ok=%v err=%v", ok, getErr)
	}
	if deploymentRow.Status != "failed" {
		t.Fatalf("deployment status = %q, want failed", deploymentRow.Status)
	}
}

func TestChaos_HealthFailure_PreviousTiersUntouched(t *testing.T) {
	h := newChaosHarness(t, "A")
	nodeA := h.node("A")

	plan := deploy.DeployPlan{
		Namespace: "frontend",
		DeployID:  "deploy-two-tier",
		Tiers: []deploy.Tier{
			{
				Services: []deploy.ServicePlan{{
					Name: "postgres",
					Create: []deploy.PlanEntry{{
						MachineID:     "A",
						ContainerName: "ployz-frontend-postgres-a001",
						Spec: deploy.ServiceSpec{
							Name:  "postgres",
							Image: "postgres:16",
						},
					}},
				}},
			},
			{
				Services: []deploy.ServicePlan{{
					Name: "app",
					HealthCheck: &deploy.HealthCheck{
						Test: []string{"CMD", "true"},
					},
					Create: []deploy.PlanEntry{{
						MachineID:     "A",
						ContainerName: "ployz-frontend-app-a001",
						Spec: deploy.ServiceSpec{
							Name:  "app",
							Image: "ghcr.io/example/app:v2",
						},
					}},
				}},
			},
		},
	}

	nodeA.health.SetUnhealthy("ployz-frontend-app-a001", errors.New("app did not pass health check"))

	_, err := deploy.ApplyPlan(
		context.Background(),
		nodeA.runtime,
		deploy.Stores{Containers: nodeA.containers, Deployments: nodeA.deploys},
		nodeA.health,
		nodeA.reader,
		plan,
		"A",
		h.clock,
		nil,
	)
	if err == nil {
		t.Fatal("ApplyPlan() expected health failure")
	}
	assertDeployErrorPhase(t, err, "health")
	mustRunning(t, nodeA.runtime, "ployz-frontend-postgres-a001")
	mustNotExist(t, nodeA.runtime, "ployz-frontend-app-a001")

	rows, listErr := nodeA.containers.ListContainersByNamespace(context.Background(), "frontend")
	if listErr != nil {
		t.Fatalf("ListContainersByNamespace() error = %v", listErr)
	}
	if len(rows) != 1 || rows[0].Service != "postgres" {
		t.Fatalf("rows after rollback = %+v, want one postgres row", rows)
	}
}

func TestChaos_OwnershipRace_BothSidesFail(t *testing.T) {
	h := newChaosHarness(t, "A", "B", "C")

	for _, from := range []string{"A", "B", "C"} {
		for _, to := range []string{"A", "B", "C"} {
			if from == to {
				continue
			}
			h.cluster.SetLink(from, to, fakecluster.LinkConfig{Latency: 20 * time.Second})
		}
	}

	planA := deploy.DeployPlan{
		Namespace: "frontend",
		DeployID:  "deploy-race",
		Tiers: []deploy.Tier{
			{
				Services: []deploy.ServicePlan{{
					Name: "api",
					HealthCheck: &deploy.HealthCheck{
						Test: []string{"CMD", "true"},
					},
					Create: []deploy.PlanEntry{{
						MachineID:     "A",
						ContainerName: "ployz-frontend-api-a001",
						Spec: deploy.ServiceSpec{
							Name:  "api",
							Image: "ghcr.io/example/api:v1",
						},
					}},
				}},
			},
			{},
		},
	}
	planB := deploy.DeployPlan{
		Namespace: "frontend",
		DeployID:  "deploy-race",
		Tiers: []deploy.Tier{
			{
				Services: []deploy.ServicePlan{{
					Name: "api",
					HealthCheck: &deploy.HealthCheck{
						Test: []string{"CMD", "true"},
					},
					Create: []deploy.PlanEntry{{
						MachineID:     "B",
						ContainerName: "ployz-frontend-api-b001",
						Spec: deploy.ServiceSpec{
							Name:  "api",
							Image: "ghcr.io/example/api:v1",
						},
					}},
				}},
			},
			{},
		},
	}

	nodeA := h.node("A")
	nodeB := h.node("B")

	var reached sync.WaitGroup
	reached.Add(2)
	release := make(chan struct{})

	waitFn := func(ctx context.Context, containerName string) error {
		reached.Done()
		select {
		case <-release:
			return nil
		case <-ctx.Done():
			return ctx.Err()
		}
	}
	nodeA.health.WaitHealthyErr = waitFn
	nodeB.health.WaitHealthyErr = waitFn
	nodeA.health.SetHealthy("ployz-frontend-api-a001")
	nodeB.health.SetHealthy("ployz-frontend-api-b001")

	type applyResult struct {
		err error
	}
	results := make(chan applyResult, 2)

	go func() {
		_, err := deploy.ApplyPlan(
			context.Background(),
			nodeA.runtime,
			deploy.Stores{Containers: nodeA.containers, Deployments: nodeA.deploys},
			nodeA.health,
			nodeA.reader,
			planA,
			"A",
			h.clock,
			nil,
		)
		results <- applyResult{err: err}
	}()

	go func() {
		_, err := deploy.ApplyPlan(
			context.Background(),
			nodeB.runtime,
			deploy.Stores{Containers: nodeB.containers, Deployments: nodeB.deploys},
			nodeB.health,
			nodeB.reader,
			planB,
			"B",
			h.clock,
			nil,
		)
		results <- applyResult{err: err}
	}()

	reached.Wait()
	h.clock.Advance(20 * time.Second)
	h.cluster.Tick()
	close(release)

	resA := <-results
	resB := <-results

	assertDeployErrorPhase(t, resA.err, "ownership")
	assertDeployErrorPhase(t, resB.err, "ownership")

	rowA, okA, errA := nodeA.deploys.GetDeployment(context.Background(), "deploy-race")
	if errA != nil || !okA {
		t.Fatalf("node A GetDeployment ok=%v err=%v", okA, errA)
	}
	rowB, okB, errB := nodeB.deploys.GetDeployment(context.Background(), "deploy-race")
	if errB != nil || !okB {
		t.Fatalf("node B GetDeployment ok=%v err=%v", okB, errB)
	}
	if rowA.Status != "failed" {
		t.Fatalf("node A deployment status = %q, want failed", rowA.Status)
	}
	if rowB.Status != "failed" {
		t.Fatalf("node B deployment status = %q, want failed", rowB.Status)
	}
}

func TestChaos_CorrosionPartitioned_DeploySucceeds(t *testing.T) {
	h := newChaosHarness(t, "A", "B", "C")

	for _, from := range []string{"A", "B", "C"} {
		for _, to := range []string{"A", "B", "C"} {
			if from == to {
				continue
			}
			h.cluster.BlockLink(from, to)
		}
	}

	nodeA := h.node("A")
	plan := deploy.DeployPlan{
		Namespace: "frontend",
		DeployID:  "deploy-partition",
		Tiers: []deploy.Tier{{
			Services: []deploy.ServicePlan{{
				Name: "app",
				Create: []deploy.PlanEntry{{
					MachineID:     "A",
					ContainerName: "ployz-frontend-app-a001",
					Spec: deploy.ServiceSpec{
						Name:  "app",
						Image: "ghcr.io/example/app:v1",
					},
				}},
			}},
		}},
	}

	_, err := deploy.ApplyPlan(
		context.Background(),
		nodeA.runtime,
		deploy.Stores{Containers: nodeA.containers, Deployments: nodeA.deploys},
		nodeA.health,
		nodeA.reader,
		plan,
		"A",
		h.clock,
		nil,
	)
	if err != nil {
		t.Fatalf("ApplyPlan() error = %v", err)
	}

	entries, err := nodeA.runtime.ContainerList(context.Background(), map[string]string{"ployz.namespace": "frontend"})
	if err != nil {
		t.Fatalf("ContainerList(A) error = %v", err)
	}
	if len(entries) != 1 || entries[0].Name != "ployz-frontend-app-a001" || !entries[0].Running {
		t.Fatalf("node A runtime entries = %+v, want running app container", entries)
	}

	if got := h.cluster.ReadContainers("B"); len(got) != 0 {
		t.Fatalf("node B rows before heal = %+v, want empty", got)
	}
	if got := h.cluster.ReadContainers("C"); len(got) != 0 {
		t.Fatalf("node C rows before heal = %+v, want empty", got)
	}

	h.cluster.Heal()
	h.cluster.Drain()

	if got := h.cluster.ReadContainers("B"); len(got) != 1 {
		t.Fatalf("node B rows after heal = %+v, want one row", got)
	}
	if got := h.cluster.ReadContainers("C"); len(got) != 1 {
		t.Fatalf("node C rows after heal = %+v, want one row", got)
	}
}

func TestChaos_ManualContainerStop_PostconditionCatches(t *testing.T) {
	h := newChaosHarness(t, "A")
	nodeA := h.node("A")

	plan := deploy.DeployPlan{
		Namespace: "frontend",
		DeployID:  "deploy-manual-stop",
		Tiers: []deploy.Tier{{
			Services: []deploy.ServicePlan{{
				Name:        "app",
				HealthCheck: &deploy.HealthCheck{Test: []string{"CMD", "true"}},
				Create: []deploy.PlanEntry{{
					MachineID:     "A",
					ContainerName: "ployz-frontend-app-a001",
					Spec:          deploy.ServiceSpec{Name: "app", Image: "ghcr.io/example/app:v1"},
				}},
			}},
		}},
	}

	nodeA.health.SetHealthy("ployz-frontend-app-a001")
	nodeA.health.WaitHealthyErr = func(ctx context.Context, containerName string) error {
		return nodeA.runtime.ContainerStop(ctx, containerName)
	}

	_, err := deploy.ApplyPlan(
		context.Background(),
		nodeA.runtime,
		deploy.Stores{Containers: nodeA.containers, Deployments: nodeA.deploys},
		nodeA.health,
		nodeA.reader,
		plan,
		"A",
		h.clock,
		nil,
	)
	if err == nil {
		t.Fatal("ApplyPlan() expected postcondition failure")
	}
	assertDeployErrorPhase(t, err, "postcondition")

	info, inspectErr := nodeA.runtime.ContainerInspect(context.Background(), "ployz-frontend-app-a001")
	if inspectErr != nil {
		t.Fatalf("ContainerInspect() error = %v", inspectErr)
	}
	if !info.Exists || info.Running {
		t.Fatalf("container state = %+v, want exists and stopped", info)
	}

	rows, listErr := nodeA.containers.ListContainersByNamespace(context.Background(), "frontend")
	if listErr != nil {
		t.Fatalf("ListContainersByNamespace() error = %v", listErr)
	}
	if len(rows) != 1 {
		t.Fatalf("rows = %+v, want one row (no rollback on halt)", rows)
	}
}

func TestChaos_StopFirst_RollbackRestartsOld(t *testing.T) {
	h := newChaosHarness(t, "A")
	nodeA := h.node("A")

	oldSpec := deploy.ServiceSpec{
		Name:  "api",
		Image: "ghcr.io/example/api:v1",
		Ports: []deploy.PortMapping{{HostPort: 8080, ContainerPort: 8080, Protocol: "tcp"}},
	}
	oldRow := mustSeedRunningContainer(t, nodeA, deploy.ContainerRow{
		ID:            "deploy-old/ployz-frontend-api-a001",
		Namespace:     "frontend",
		DeployID:      "deploy-old",
		Service:       "api",
		MachineID:     "A",
		ContainerName: "ployz-frontend-api-a001",
		Status:        "running",
		CreatedAt:     "2026-01-01T00:00:00Z",
		UpdatedAt:     "2026-01-01T00:00:00Z",
		Version:       1,
	}, oldSpec)

	newSpec := deploy.ServiceSpec{
		Name:  "api",
		Image: "ghcr.io/example/api:v2",
		Ports: []deploy.PortMapping{{HostPort: 8080, ContainerPort: 8080, Protocol: "tcp"}},
	}
	plan := deploy.DeployPlan{
		Namespace: "frontend",
		DeployID:  "deploy-stop-first",
		Tiers: []deploy.Tier{{
			Services: []deploy.ServicePlan{{
				Name:        "api",
				HealthCheck: &deploy.HealthCheck{Test: []string{"CMD", "true"}},
				NeedsRecreate: []deploy.PlanEntry{{
					MachineID:     "A",
					ContainerName: "ployz-frontend-api-a001",
					Spec:          newSpec,
					CurrentRow:    &oldRow,
				}},
			}},
		}},
	}

	nodeA.health.SetUnhealthy("ployz-frontend-api-a001", errors.New("new container unhealthy"))

	_, err := deploy.ApplyPlan(
		context.Background(),
		nodeA.runtime,
		deploy.Stores{Containers: nodeA.containers, Deployments: nodeA.deploys},
		nodeA.health,
		nodeA.reader,
		plan,
		"A",
		h.clock,
		nil,
	)
	if err == nil {
		t.Fatal("ApplyPlan() expected health failure")
	}
	assertDeployErrorPhase(t, err, "health")
	mustRunning(t, nodeA.runtime, "ployz-frontend-api-a001")

	rows, listErr := nodeA.containers.ListContainersByNamespace(context.Background(), "frontend")
	if listErr != nil {
		t.Fatalf("ListContainersByNamespace() error = %v", listErr)
	}
	if len(rows) != 1 || rows[0].DeployID != "deploy-old" {
		t.Fatalf("rows after rollback = %+v, want restored old row", rows)
	}
}

func TestChaos_StartFirst_RollbackRemovesNew(t *testing.T) {
	h := newChaosHarness(t, "A")
	nodeA := h.node("A")

	oldSpec := deploy.ServiceSpec{Name: "api", Image: "ghcr.io/example/api:v1"}
	oldRow := mustSeedRunningContainer(t, nodeA, deploy.ContainerRow{
		ID:            "deploy-old/ployz-frontend-api-a001",
		Namespace:     "frontend",
		DeployID:      "deploy-old",
		Service:       "api",
		MachineID:     "A",
		ContainerName: "ployz-frontend-api-a001",
		Status:        "running",
		CreatedAt:     "2026-01-01T00:00:00Z",
		UpdatedAt:     "2026-01-01T00:00:00Z",
		Version:       1,
	}, oldSpec)

	plan := deploy.DeployPlan{
		Namespace: "frontend",
		DeployID:  "deploy-start-first",
		Tiers: []deploy.Tier{{
			Services: []deploy.ServicePlan{{
				Name:        "api",
				HealthCheck: &deploy.HealthCheck{Test: []string{"CMD", "true"}},
				NeedsRecreate: []deploy.PlanEntry{{
					MachineID:     "A",
					ContainerName: "ployz-frontend-api-a002",
					Spec: deploy.ServiceSpec{
						Name:  "api",
						Image: "ghcr.io/example/api:v2",
					},
					CurrentRow: &oldRow,
				}},
			}},
		}},
	}

	nodeA.health.SetUnhealthy("ployz-frontend-api-a002", errors.New("new container unhealthy"))

	_, err := deploy.ApplyPlan(
		context.Background(),
		nodeA.runtime,
		deploy.Stores{Containers: nodeA.containers, Deployments: nodeA.deploys},
		nodeA.health,
		nodeA.reader,
		plan,
		"A",
		h.clock,
		nil,
	)
	if err == nil {
		t.Fatal("ApplyPlan() expected health failure")
	}
	assertDeployErrorPhase(t, err, "health")
	mustRunning(t, nodeA.runtime, "ployz-frontend-api-a001")
	mustNotExist(t, nodeA.runtime, "ployz-frontend-api-a002")

	rows, listErr := nodeA.containers.ListContainersByNamespace(context.Background(), "frontend")
	if listErr != nil {
		t.Fatalf("ListContainersByNamespace() error = %v", listErr)
	}
	if len(rows) != 1 || rows[0].ContainerName != "ployz-frontend-api-a001" {
		t.Fatalf("rows after rollback = %+v, want old row only", rows)
	}
}

func TestChaos_ScaleUp_PartialFailure(t *testing.T) {
	h := newChaosHarness(t, "A")
	nodeA := h.node("A")

	oldSpec := deploy.ServiceSpec{Name: "api", Image: "ghcr.io/example/api:v1"}
	_ = mustSeedRunningContainer(t, nodeA, deploy.ContainerRow{
		ID:            "deploy-old/ployz-frontend-api-a001",
		Namespace:     "frontend",
		DeployID:      "deploy-old",
		Service:       "api",
		MachineID:     "A",
		ContainerName: "ployz-frontend-api-a001",
		Status:        "running",
		CreatedAt:     "2026-01-01T00:00:00Z",
		UpdatedAt:     "2026-01-01T00:00:00Z",
		Version:       1,
	}, oldSpec)
	_ = mustSeedRunningContainer(t, nodeA, deploy.ContainerRow{
		ID:            "deploy-old/ployz-frontend-api-a002",
		Namespace:     "frontend",
		DeployID:      "deploy-old",
		Service:       "api",
		MachineID:     "A",
		ContainerName: "ployz-frontend-api-a002",
		Status:        "running",
		CreatedAt:     "2026-01-01T00:00:00Z",
		UpdatedAt:     "2026-01-01T00:00:00Z",
		Version:       1,
	}, oldSpec)

	plan := deploy.DeployPlan{
		Namespace: "frontend",
		DeployID:  "deploy-scale-up",
		Tiers: []deploy.Tier{{
			Services: []deploy.ServicePlan{{
				Name: "api",
				UpToDate: []deploy.PlanEntry{
					{MachineID: "A", ContainerName: "ployz-frontend-api-a001", Spec: oldSpec},
					{MachineID: "A", ContainerName: "ployz-frontend-api-a002", Spec: oldSpec},
				},
				HealthCheck: &deploy.HealthCheck{Test: []string{"CMD", "true"}},
				Create: []deploy.PlanEntry{
					{MachineID: "A", ContainerName: "ployz-frontend-api-a003", Spec: deploy.ServiceSpec{Name: "api", Image: "ghcr.io/example/api:v2"}},
					{MachineID: "A", ContainerName: "ployz-frontend-api-a004", Spec: deploy.ServiceSpec{Name: "api", Image: "ghcr.io/example/api:v2"}},
					{MachineID: "A", ContainerName: "ployz-frontend-api-a005", Spec: deploy.ServiceSpec{Name: "api", Image: "ghcr.io/example/api:v2"}},
				},
			}},
		}},
	}

	nodeA.health.SetHealthy("ployz-frontend-api-a003")
	nodeA.health.SetHealthy("ployz-frontend-api-a004")
	nodeA.health.SetUnhealthy("ployz-frontend-api-a005", errors.New("one new replica failed"))

	_, err := deploy.ApplyPlan(
		context.Background(),
		nodeA.runtime,
		deploy.Stores{Containers: nodeA.containers, Deployments: nodeA.deploys},
		nodeA.health,
		nodeA.reader,
		plan,
		"A",
		h.clock,
		nil,
	)
	if err == nil {
		t.Fatal("ApplyPlan() expected health failure")
	}
	assertDeployErrorPhase(t, err, "health")

	mustRunning(t, nodeA.runtime, "ployz-frontend-api-a001")
	mustRunning(t, nodeA.runtime, "ployz-frontend-api-a002")
	mustNotExist(t, nodeA.runtime, "ployz-frontend-api-a003")
	mustNotExist(t, nodeA.runtime, "ployz-frontend-api-a004")
	mustNotExist(t, nodeA.runtime, "ployz-frontend-api-a005")

	rows, listErr := nodeA.containers.ListContainersByNamespace(context.Background(), "frontend")
	if listErr != nil {
		t.Fatalf("ListContainersByNamespace() error = %v", listErr)
	}
	if len(rows) != 2 {
		t.Fatalf("rows after rollback = %+v, want only two original rows", rows)
	}
}

func mustSeedRunningContainer(t *testing.T, node *chaosNode, row deploy.ContainerRow, spec deploy.ServiceSpec) deploy.ContainerRow {
	t.Helper()

	specJSON, err := json.Marshal(spec)
	if err != nil {
		t.Fatalf("json.Marshal(spec) error = %v", err)
	}
	row.SpecJSON = string(specJSON)

	cfg := mesh.ContainerCreateConfig{
		Name:  row.ContainerName,
		Image: spec.Image,
		Labels: map[string]string{
			"ployz.namespace":  row.Namespace,
			"ployz.service":    row.Service,
			"ployz.deploy_id":  row.DeployID,
			"ployz.machine_id": row.MachineID,
		},
	}
	if err := node.runtime.ContainerCreate(context.Background(), cfg); err != nil {
		t.Fatalf("ContainerCreate(%s) error = %v", row.ContainerName, err)
	}
	if err := node.runtime.ContainerStart(context.Background(), row.ContainerName); err != nil {
		t.Fatalf("ContainerStart(%s) error = %v", row.ContainerName, err)
	}
	if err := node.containers.InsertContainer(context.Background(), row); err != nil {
		t.Fatalf("InsertContainer(%s) error = %v", row.ID, err)
	}
	return row
}

func mustRunning(t *testing.T, rt *fakeleaf.ContainerRuntime, name string) {
	t.Helper()
	info, err := rt.ContainerInspect(context.Background(), name)
	if err != nil {
		t.Fatalf("ContainerInspect(%s) error = %v", name, err)
	}
	if !info.Exists || !info.Running {
		t.Fatalf("container %s state = %+v, want running", name, info)
	}
}

func mustNotExist(t *testing.T, rt *fakeleaf.ContainerRuntime, name string) {
	t.Helper()
	info, err := rt.ContainerInspect(context.Background(), name)
	if err != nil {
		t.Fatalf("ContainerInspect(%s) error = %v", name, err)
	}
	if info.Exists {
		t.Fatalf("container %s exists = true, want false", name)
	}
}

func assertDeployErrorPhase(t *testing.T, err error, phase string) {
	t.Helper()
	if err == nil {
		t.Fatalf("expected deploy error phase %q, got nil", phase)
	}
	var de *deploy.DeployError
	if !errors.As(err, &de) {
		t.Fatalf("error type = %T, want *deploy.DeployError", err)
	}
	if de.Phase != phase {
		t.Fatalf("DeployError.Phase = %q, want %q", de.Phase, phase)
	}
}
