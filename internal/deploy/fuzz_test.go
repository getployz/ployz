package deploy_test

import (
	"context"
	"sync"
	"testing"
	"time"

	fakecluster "ployz/internal/adapter/fake/cluster"
	"ployz/internal/deploy"
)

func FuzzApplyPlan_NeverHalfReplaced(f *testing.F) {
	f.Add([]byte{0})
	f.Add([]byte{1})

	f.Fuzz(func(t *testing.T, data []byte) {
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
			DeployID:  "deploy-fuzz-recreate",
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

		healthFails := len(data) > 0 && data[0]&1 == 1
		if healthFails {
			nodeA.health.SetUnhealthy("ployz-frontend-api-a002", context.DeadlineExceeded)
		} else {
			nodeA.health.SetHealthy("ployz-frontend-api-a002")
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
		if healthFails {
			if err == nil {
				t.Fatal("ApplyPlan() expected health failure")
			}
			assertDeployErrorPhase(t, err, "health")
		} else if err != nil {
			t.Fatalf("ApplyPlan() error = %v", err)
		}

		oldInfo, oldErr := nodeA.runtime.ContainerInspect(context.Background(), "ployz-frontend-api-a001")
		if oldErr != nil {
			t.Fatalf("ContainerInspect(old) error = %v", oldErr)
		}
		newInfo, newErr := nodeA.runtime.ContainerInspect(context.Background(), "ployz-frontend-api-a002")
		if newErr != nil {
			t.Fatalf("ContainerInspect(new) error = %v", newErr)
		}

		running := 0
		if oldInfo.Exists && oldInfo.Running {
			running++
		}
		if newInfo.Exists && newInfo.Running {
			running++
		}
		if running != 1 {
			t.Fatalf("half-replaced invariant violated: old=%+v new=%+v", oldInfo, newInfo)
		}
	})
}

func FuzzApplyPlan_NoOrphanedContainers(f *testing.F) {
	f.Add([]byte{0})
	f.Add([]byte{1})

	f.Fuzz(func(t *testing.T, data []byte) {
		h := newChaosHarness(t, "A")
		nodeA := h.node("A")

		plan := deploy.DeployPlan{
			Namespace: "frontend",
			DeployID:  "deploy-fuzz-orphans",
			Tiers: []deploy.Tier{{
				Services: []deploy.ServicePlan{{
					Name:        "app",
					HealthCheck: &deploy.HealthCheck{Test: []string{"CMD", "true"}},
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

		healthFails := len(data) > 0 && data[0]&1 == 1
		if healthFails {
			nodeA.health.SetUnhealthy("ployz-frontend-app-a001", context.DeadlineExceeded)
		} else {
			nodeA.health.SetHealthy("ployz-frontend-app-a001")
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
		if healthFails {
			if err == nil {
				t.Fatal("ApplyPlan() expected health failure")
			}
			assertDeployErrorPhase(t, err, "health")
		} else if err != nil {
			t.Fatalf("ApplyPlan() error = %v", err)
		}

		runtimeEntries, listErr := nodeA.runtime.ContainerList(context.Background(), map[string]string{"ployz.namespace": "frontend"})
		if listErr != nil {
			t.Fatalf("ContainerList() error = %v", listErr)
		}
		rows, rowsErr := nodeA.containers.ListContainersByNamespace(context.Background(), "frontend")
		if rowsErr != nil {
			t.Fatalf("ListContainersByNamespace() error = %v", rowsErr)
		}

		runningNames := make(map[string]struct{}, len(runtimeEntries))
		for _, entry := range runtimeEntries {
			if entry.Running {
				runningNames[entry.Name] = struct{}{}
			}
		}

		rowNames := make(map[string]struct{}, len(rows))
		for _, row := range rows {
			if row.Status == "running" {
				rowNames[row.ContainerName] = struct{}{}
			}
		}

		if len(runningNames) != len(rowNames) {
			t.Fatalf("orphan/ghost invariant violated: running=%v rows=%v", runningNames, rowNames)
		}
		for name := range runningNames {
			if _, ok := rowNames[name]; !ok {
				t.Fatalf("running container %q has no row; rows=%v", name, rowNames)
			}
		}
		for name := range rowNames {
			if _, ok := runningNames[name]; !ok {
				t.Fatalf("row %q has no running container; running=%v", name, runningNames)
			}
		}
	})
}

func FuzzApplyPlan_BothSidesFailOnContention(f *testing.F) {
	f.Add(byte(0))
	f.Add(byte(17))

	f.Fuzz(func(t *testing.T, latencyByte byte) {
		h := newChaosHarness(t, "A", "B")
		nodeA := h.node("A")
		nodeB := h.node("B")

		latency := time.Duration(1+int(latencyByte%30)) * time.Second
		h.cluster.SetLink("A", "B", fakecluster.LinkConfig{Latency: latency})
		h.cluster.SetLink("B", "A", fakecluster.LinkConfig{Latency: latency})

		planA := deploy.DeployPlan{
			Namespace: "frontend",
			DeployID:  "deploy-fuzz-race",
			Tiers: []deploy.Tier{{
				Services: []deploy.ServicePlan{{
					Name:        "api",
					HealthCheck: &deploy.HealthCheck{Test: []string{"CMD", "true"}},
					Create: []deploy.PlanEntry{{
						MachineID:     "A",
						ContainerName: "ployz-frontend-api-a001",
						Spec: deploy.ServiceSpec{
							Name:  "api",
							Image: "ghcr.io/example/api:v1",
						},
					}},
				}},
			}, {}},
		}
		planB := deploy.DeployPlan{
			Namespace: "frontend",
			DeployID:  "deploy-fuzz-race",
			Tiers: []deploy.Tier{{
				Services: []deploy.ServicePlan{{
					Name:        "api",
					HealthCheck: &deploy.HealthCheck{Test: []string{"CMD", "true"}},
					Create: []deploy.PlanEntry{{
						MachineID:     "B",
						ContainerName: "ployz-frontend-api-b001",
						Spec: deploy.ServiceSpec{
							Name:  "api",
							Image: "ghcr.io/example/api:v1",
						},
					}},
				}},
			}, {}},
		}

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

		results := make(chan error, 2)

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
			results <- err
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
			results <- err
		}()

		reached.Wait()
		h.clock.Advance(latency)
		h.cluster.Tick()
		close(release)

		errA := <-results
		errB := <-results
		assertDeployErrorPhase(t, errA, "ownership")
		assertDeployErrorPhase(t, errB, "ownership")

		mustRunning(t, nodeA.runtime, "ployz-frontend-api-a001")
		mustRunning(t, nodeB.runtime, "ployz-frontend-api-b001")
	})
}

func FuzzPostcondition_AlwaysDetectsMismatch(f *testing.F) {
	f.Add("api-1", "api:1", true, "api:1")
	f.Add("api-1", "api:2", true, "api:1")
	f.Add("api-1", "api:1", false, "api:1")

	f.Fuzz(func(t *testing.T, containerName, actualImage string, running bool, expectedImage string) {
		if containerName == "" {
			t.Skip()
		}

		actual := []deploy.ContainerState{{
			ContainerName: containerName,
			Image:         actualImage,
			Running:       running,
		}}
		expected := []deploy.ContainerResult{{
			MachineID:     "m1",
			ContainerName: containerName,
			Expected:      expectedImage,
		}}

		err := deploy.AssertTierState(actual, expected)
		wantMatch := running && actualImage == expectedImage
		if wantMatch && err != nil {
			t.Fatalf("AssertTierState() = %v, want nil (running and image match)", err)
		}
		if !wantMatch && err == nil {
			t.Fatalf("AssertTierState() = nil, want mismatch error (running=%v actual=%q expected=%q)", running, actualImage, expectedImage)
		}
	})
}
