package deploy_test

import (
	"context"
	"testing"
	"time"

	fakecluster "ployz/internal/adapter/fake/cluster"
	fakeleaf "ployz/internal/adapter/fake/leaf"
	"ployz/internal/deploy"
)

func TestClusterDeploy_ThreeNodes_Convergence(t *testing.T) {
	clock := fakeleaf.NewClock(time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC))
	cluster := fakecluster.NewCluster(clock)

	runtimeA := fakeleaf.NewContainerRuntime()
	storeA := fakecluster.NewClusterContainerStore(cluster, "A")
	deployStoreA := fakecluster.NewClusterDeploymentStore(cluster, "A")
	healthA := fakeleaf.NewHealthChecker()
	readerA := &runtimeStateReader{runtime: runtimeA}

	_ = fakecluster.NewClusterContainerStore(cluster, "B")
	_ = fakecluster.NewClusterContainerStore(cluster, "C")
	_ = fakecluster.NewClusterDeploymentStore(cluster, "B")
	_ = fakecluster.NewClusterDeploymentStore(cluster, "C")

	cluster.SetLink("A", "B", fakecluster.LinkConfig{Latency: 500 * time.Millisecond})
	cluster.SetLink("A", "C", fakecluster.LinkConfig{Latency: 500 * time.Millisecond})

	plan := deploy.DeployPlan{
		Namespace: "web",
		DeployID:  "deploy-converge",
		Tiers: []deploy.Tier{{
			Services: []deploy.ServicePlan{{
				Name: "app",
				Create: []deploy.PlanEntry{{
					MachineID:     "A",
					ContainerName: "ployz-web-app-a001",
					Spec: deploy.ServiceSpec{
						Name:  "app",
						Image: "ghcr.io/example/web:v1",
					},
				}},
			}},
		}},
	}

	_, err := deploy.ApplyPlan(
		context.Background(),
		runtimeA,
		deploy.Stores{Containers: storeA, Deployments: deployStoreA},
		healthA,
		readerA,
		plan,
		"A",
		clock,
		nil,
	)
	if err != nil {
		t.Fatalf("ApplyPlan() error = %v", err)
	}

	if got := cluster.ReadContainers("A"); len(got) != 1 {
		t.Fatalf("node A containers = %+v, want one row", got)
	}
	if got := cluster.ReadContainers("B"); len(got) != 0 {
		t.Fatalf("node B containers before Tick = %+v, want empty", got)
	}
	if got := cluster.ReadContainers("C"); len(got) != 0 {
		t.Fatalf("node C containers before Tick = %+v, want empty", got)
	}

	clock.Advance(500 * time.Millisecond)
	cluster.Tick()

	if got := cluster.ReadContainers("B"); len(got) != 1 {
		t.Fatalf("node B containers after Tick = %+v, want one row", got)
	}
	if got := cluster.ReadContainers("C"); len(got) != 1 {
		t.Fatalf("node C containers after Tick = %+v, want one row", got)
	}
}

func TestClusterDeploy_Partition_HealConverge(t *testing.T) {
	clock := fakeleaf.NewClock(time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC))
	cluster := fakecluster.NewCluster(clock)

	runtimeA := fakeleaf.NewContainerRuntime()
	storeA := fakecluster.NewClusterContainerStore(cluster, "A")
	deployStoreA := fakecluster.NewClusterDeploymentStore(cluster, "A")
	readerA := &runtimeStateReader{runtime: runtimeA}

	runtimeB := fakeleaf.NewContainerRuntime()
	storeB := fakecluster.NewClusterContainerStore(cluster, "B")
	deployStoreB := fakecluster.NewClusterDeploymentStore(cluster, "B")
	readerB := &runtimeStateReader{runtime: runtimeB}

	_ = fakecluster.NewClusterContainerStore(cluster, "C")
	_ = fakecluster.NewClusterDeploymentStore(cluster, "C")

	cluster.Partition([]string{"A", "B"}, []string{"C"})

	planA := deploy.DeployPlan{
		Namespace: "web",
		DeployID:  "deploy-partition-a",
		Tiers: []deploy.Tier{{
			Services: []deploy.ServicePlan{{
				Name: "app",
				Create: []deploy.PlanEntry{{
					MachineID:     "A",
					ContainerName: "ployz-web-app-a001",
					Spec:          deploy.ServiceSpec{Name: "app", Image: "ghcr.io/example/web:v1"},
				}},
			}},
		}},
	}
	planB := deploy.DeployPlan{
		Namespace: "web",
		DeployID:  "deploy-partition-b",
		Tiers: []deploy.Tier{{
			Services: []deploy.ServicePlan{{
				Name: "app",
				Create: []deploy.PlanEntry{{
					MachineID:     "B",
					ContainerName: "ployz-web-app-b001",
					Spec:          deploy.ServiceSpec{Name: "app", Image: "ghcr.io/example/web:v1"},
				}},
			}},
		}},
	}

	if _, err := deploy.ApplyPlan(
		context.Background(),
		runtimeA,
		deploy.Stores{Containers: storeA, Deployments: deployStoreA},
		fakeleaf.NewHealthChecker(),
		readerA,
		planA,
		"A",
		clock,
		nil,
	); err != nil {
		t.Fatalf("ApplyPlan(A) error = %v", err)
	}
	if _, err := deploy.ApplyPlan(
		context.Background(),
		runtimeB,
		deploy.Stores{Containers: storeB, Deployments: deployStoreB},
		fakeleaf.NewHealthChecker(),
		readerB,
		planB,
		"B",
		clock,
		nil,
	); err != nil {
		t.Fatalf("ApplyPlan(B) error = %v", err)
	}

	if got := cluster.ReadContainers("C"); len(got) != 0 {
		t.Fatalf("node C rows before heal = %+v, want empty", got)
	}

	cluster.Heal()
	cluster.Drain()

	if got := cluster.ReadContainers("C"); len(got) != 2 {
		t.Fatalf("node C rows after heal = %+v, want two rows", got)
	}
}

func TestClusterDeploy_NodeRestart_MergesState(t *testing.T) {
	clock := fakeleaf.NewClock(time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC))
	cluster := fakecluster.NewCluster(clock)

	_ = fakecluster.NewClusterContainerStore(cluster, "A")
	_ = fakecluster.NewClusterContainerStore(cluster, "B")
	_ = fakecluster.NewClusterContainerStore(cluster, "C")

	baseRow := deploy.ContainerRow{
		ID:            "deploy-1/ployz-web-app-a001",
		Namespace:     "web",
		DeployID:      "deploy-1",
		Service:       "app",
		MachineID:     "A",
		ContainerName: "ployz-web-app-a001",
		SpecJSON:      `{"name":"app","image":"ghcr.io/example/web:v1"}`,
		Status:        "running",
		Version:       1,
		CreatedAt:     "2026-01-01T00:00:00Z",
		UpdatedAt:     "2026-01-01T00:00:00Z",
	}
	cluster.WriteContainer("A", baseRow)
	cluster.Drain()

	cluster.KillNode("B")

	newRow := deploy.ContainerRow{
		ID:            "deploy-2/ployz-web-app-a002",
		Namespace:     "web",
		DeployID:      "deploy-2",
		Service:       "app",
		MachineID:     "A",
		ContainerName: "ployz-web-app-a002",
		SpecJSON:      `{"name":"app","image":"ghcr.io/example/web:v2"}`,
		Status:        "running",
		Version:       1,
		CreatedAt:     "2026-01-01T00:01:00Z",
		UpdatedAt:     "2026-01-01T00:01:00Z",
	}
	cluster.WriteContainer("A", newRow)
	cluster.Drain()

	if got := cluster.ReadContainers("B"); len(got) != 1 {
		t.Fatalf("node B rows while killed = %+v, want one base row", got)
	}

	cluster.RestartNode("B")

	if got := cluster.ReadContainers("B"); len(got) != 2 {
		t.Fatalf("node B rows after restart = %+v, want merged rows", got)
	}
}

func TestClusterDeploy_DeleteReplication(t *testing.T) {
	clock := fakeleaf.NewClock(time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC))
	cluster := fakecluster.NewCluster(clock)

	storeA := fakecluster.NewClusterContainerStore(cluster, "A")
	_ = fakecluster.NewClusterContainerStore(cluster, "B")
	_ = fakecluster.NewClusterContainerStore(cluster, "C")

	row := deploy.ContainerRow{
		ID:            "deploy-1/ployz-web-app-a001",
		Namespace:     "web",
		DeployID:      "deploy-1",
		Service:       "app",
		MachineID:     "A",
		ContainerName: "ployz-web-app-a001",
		SpecJSON:      `{"name":"app","image":"ghcr.io/example/web:v1"}`,
		Status:        "running",
		Version:       1,
		CreatedAt:     "2026-01-01T00:00:00Z",
		UpdatedAt:     "2026-01-01T00:00:00Z",
	}
	if err := storeA.InsertContainer(context.Background(), row); err != nil {
		t.Fatalf("InsertContainer() error = %v", err)
	}
	cluster.Drain()

	if err := storeA.DeleteContainer(context.Background(), row.ID); err != nil {
		t.Fatalf("DeleteContainer() error = %v", err)
	}
	cluster.Drain()

	if got := cluster.ReadContainers("B"); len(got) != 0 {
		t.Fatalf("node B rows after delete = %+v, want empty", got)
	}
	if got := cluster.ReadContainers("C"); len(got) != 0 {
		t.Fatalf("node C rows after delete = %+v, want empty", got)
	}
}

func TestClusterDeploy_OwnershipReplication(t *testing.T) {
	clock := fakeleaf.NewClock(time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC))
	cluster := fakecluster.NewCluster(clock)

	storeA := fakecluster.NewClusterDeploymentStore(cluster, "A")
	storeB := fakecluster.NewClusterDeploymentStore(cluster, "B")
	_ = fakecluster.NewClusterDeploymentStore(cluster, "C")

	cluster.SetLink("A", "B", fakecluster.LinkConfig{Latency: 500 * time.Millisecond})
	cluster.SetLink("A", "C", fakecluster.LinkConfig{Latency: 500 * time.Millisecond})

	row := deploy.DeploymentRow{
		ID:             "deploy-own",
		Namespace:      "web",
		SpecJSON:       "{}",
		Status:         "in_progress",
		Owner:          "",
		OwnerHeartbeat: "",
		Version:        1,
		CreatedAt:      "2026-01-01T00:00:00Z",
		UpdatedAt:      "2026-01-01T00:00:00Z",
	}
	if err := storeA.InsertDeployment(context.Background(), row); err != nil {
		t.Fatalf("InsertDeployment() error = %v", err)
	}
	cluster.Drain()

	if err := storeA.AcquireOwnership(context.Background(), "deploy-own", "A", "2026-01-01T00:00:01Z"); err != nil {
		t.Fatalf("AcquireOwnership(A) error = %v", err)
	}

	bBefore, okBefore, errBefore := storeB.GetDeployment(context.Background(), "deploy-own")
	if errBefore != nil || !okBefore {
		t.Fatalf("GetDeployment(B) before tick ok=%v err=%v", okBefore, errBefore)
	}
	if bBefore.Owner != "" {
		t.Fatalf("owner before tick = %q, want empty", bBefore.Owner)
	}

	clock.Advance(500 * time.Millisecond)
	cluster.Tick()

	bAfter, okAfter, errAfter := storeB.GetDeployment(context.Background(), "deploy-own")
	if errAfter != nil || !okAfter {
		t.Fatalf("GetDeployment(B) after tick ok=%v err=%v", okAfter, errAfter)
	}
	if bAfter.Owner != "A" {
		t.Fatalf("owner after tick = %q, want A", bAfter.Owner)
	}

	if err := storeB.AcquireOwnership(context.Background(), "deploy-own", "B", "2026-01-01T00:00:02Z"); err == nil {
		t.Fatal("AcquireOwnership(B) expected ownership conflict")
	}
}
