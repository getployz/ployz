package deploy_test

import (
	"context"
	"errors"
	"sync"
	"testing"
	"time"

	fakeleaf "ployz/internal/adapter/fake/leaf"
	"ployz/internal/deploy"
)

type staticStateReader struct {
	mu      sync.Mutex
	states  map[string][]deploy.ContainerState
	err     error
	callCnt int
}

func (s *staticStateReader) ReadMachineState(ctx context.Context, machineID, namespace string) ([]deploy.ContainerState, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.callCnt++
	if s.err != nil {
		return nil, s.err
	}
	out := append([]deploy.ContainerState(nil), s.states[machineID]...)
	return out, nil
}

func (s *staticStateReader) Calls() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.callCnt
}

func TestApplyPlan_HappyPath(t *testing.T) {
	clock := fakeleaf.NewClock(time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC))
	rt := fakeleaf.NewContainerRuntime()
	containerStore := fakeleaf.NewContainerStore()
	deploymentStore := fakeleaf.NewDeploymentStore()
	health := fakeleaf.NewHealthChecker()
	stateReader := &staticStateReader{states: map[string][]deploy.ContainerState{
		"m1": {{ContainerName: "ployz-ns-api-a001", Image: "api:1", Running: true}},
	}}
	events := make(chan deploy.ProgressEvent, 64)

	plan := deploy.DeployPlan{
		Namespace: "ns",
		DeployID:  "deploy-1",
		Tiers: []deploy.Tier{{
			Services: []deploy.ServicePlan{{
				Name: "api",
				Create: []deploy.PlanEntry{{
					MachineID:     "m1",
					ContainerName: "ployz-ns-api-a001",
					Spec: deploy.ServiceSpec{
						Name:  "api",
						Image: "api:1",
					},
				}},
			}},
		}},
	}

	result, err := deploy.ApplyPlan(
		context.Background(),
		rt,
		deploy.Stores{Containers: containerStore, Deployments: deploymentStore},
		health,
		stateReader,
		plan,
		"m1",
		clock,
		events,
	)
	if err != nil {
		t.Fatalf("ApplyPlan() error = %v", err)
	}

	if result.Namespace != "ns" || result.DeployID != "deploy-1" {
		t.Fatalf("ApplyPlan() result = %+v, want namespace ns deploy-1", result)
	}
	if len(result.Tiers) != 1 {
		t.Fatalf("tiers len = %d, want 1", len(result.Tiers))
	}
	if result.Tiers[0].Status != "completed" {
		t.Fatalf("tier status = %q, want completed", result.Tiers[0].Status)
	}

	rows, err := containerStore.ListContainersByNamespace(context.Background(), "ns")
	if err != nil {
		t.Fatalf("ListContainersByNamespace() error = %v", err)
	}
	if len(rows) != 1 || rows[0].ContainerName != "ployz-ns-api-a001" {
		t.Fatalf("container rows = %+v, want one api row", rows)
	}

	deployment, ok, err := deploymentStore.GetDeployment(context.Background(), "deploy-1")
	if err != nil {
		t.Fatalf("GetDeployment() error = %v", err)
	}
	if !ok {
		t.Fatal("GetDeployment() found=false, want true")
	}
	if deployment.Status != "succeeded" {
		t.Fatalf("deployment status = %q, want succeeded", deployment.Status)
	}
	if deployment.Owner != "" || deployment.OwnerHeartbeat != "" {
		t.Fatalf("deployment ownership not cleared: owner=%q heartbeat=%q", deployment.Owner, deployment.OwnerHeartbeat)
	}

	info, err := rt.ContainerInspect(context.Background(), "ployz-ns-api-a001")
	if err != nil {
		t.Fatalf("ContainerInspect() error = %v", err)
	}
	if !info.Exists || !info.Running {
		t.Fatalf("ContainerInspect() = %+v, want running container", info)
	}

	if stateReader.Calls() != 1 {
		t.Fatalf("state reader calls = %d, want 1", stateReader.Calls())
	}

	types := drainEventTypes(events)
	if !contains(types, "tier_started") || !contains(types, "image_pulled") || !contains(types, "tier_complete") || !contains(types, "deploy_complete") {
		t.Fatalf("event types = %v, want tier_started,image_pulled,tier_complete,deploy_complete", types)
	}
}

func TestApplyPlan_HealthFailureRollsBackTier(t *testing.T) {
	clock := fakeleaf.NewClock(time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC))
	rt := fakeleaf.NewContainerRuntime()
	containerStore := fakeleaf.NewContainerStore()
	deploymentStore := fakeleaf.NewDeploymentStore()
	health := fakeleaf.NewHealthChecker()
	health.SetUnhealthy("ployz-ns-api-a001", errors.New("unhealthy"))
	stateReader := &staticStateReader{states: map[string][]deploy.ContainerState{}}
	events := make(chan deploy.ProgressEvent, 64)

	plan := deploy.DeployPlan{
		Namespace: "ns",
		DeployID:  "deploy-2",
		Tiers: []deploy.Tier{{
			Services: []deploy.ServicePlan{{
				Name: "api",
				HealthCheck: &deploy.HealthCheck{
					Test: []string{"CMD", "true"},
				},
				Create: []deploy.PlanEntry{{
					MachineID:     "m1",
					ContainerName: "ployz-ns-api-a001",
					Spec: deploy.ServiceSpec{
						Name:  "api",
						Image: "api:2",
					},
				}},
			}},
		}},
	}

	_, err := deploy.ApplyPlan(
		context.Background(),
		rt,
		deploy.Stores{Containers: containerStore, Deployments: deploymentStore},
		health,
		stateReader,
		plan,
		"m1",
		clock,
		events,
	)
	if err == nil {
		t.Fatal("ApplyPlan() expected error")
	}

	var de *deploy.DeployError
	if !errors.As(err, &de) {
		t.Fatalf("ApplyPlan() error = %T, want *deploy.DeployError", err)
	}
	if de.Phase != "health" {
		t.Fatalf("DeployError.Phase = %q, want health", de.Phase)
	}

	info, inspectErr := rt.ContainerInspect(context.Background(), "ployz-ns-api-a001")
	if inspectErr != nil {
		t.Fatalf("ContainerInspect() error = %v", inspectErr)
	}
	if info.Exists {
		t.Fatalf("container exists after rollback = %v, want false", info.Exists)
	}

	rows, listErr := containerStore.ListContainersByNamespace(context.Background(), "ns")
	if listErr != nil {
		t.Fatalf("ListContainersByNamespace() error = %v", listErr)
	}
	if len(rows) != 0 {
		t.Fatalf("container rows after rollback = %+v, want empty", rows)
	}

	deployment, ok, getErr := deploymentStore.GetDeployment(context.Background(), "deploy-2")
	if getErr != nil || !ok {
		t.Fatalf("GetDeployment() ok=%v err=%v", ok, getErr)
	}
	if deployment.Status != "failed" {
		t.Fatalf("deployment status = %q, want failed", deployment.Status)
	}

	types := drainEventTypes(events)
	if !contains(types, "rollback_started") {
		t.Fatalf("event types = %v, want rollback_started", types)
	}
}

func TestApplyPlan_PostconditionMismatchNoRollback(t *testing.T) {
	clock := fakeleaf.NewClock(time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC))
	rt := fakeleaf.NewContainerRuntime()
	containerStore := fakeleaf.NewContainerStore()
	deploymentStore := fakeleaf.NewDeploymentStore()
	health := fakeleaf.NewHealthChecker()
	stateReader := &staticStateReader{states: map[string][]deploy.ContainerState{
		"m1": {},
	}}
	events := make(chan deploy.ProgressEvent, 64)

	plan := deploy.DeployPlan{
		Namespace: "ns",
		DeployID:  "deploy-3",
		Tiers: []deploy.Tier{{
			Services: []deploy.ServicePlan{{
				Name: "api",
				Create: []deploy.PlanEntry{{
					MachineID:     "m1",
					ContainerName: "ployz-ns-api-a001",
					Spec: deploy.ServiceSpec{
						Name:  "api",
						Image: "api:3",
					},
				}},
			}},
		}},
	}

	_, err := deploy.ApplyPlan(
		context.Background(),
		rt,
		deploy.Stores{Containers: containerStore, Deployments: deploymentStore},
		health,
		stateReader,
		plan,
		"m1",
		clock,
		events,
	)
	if err == nil {
		t.Fatal("ApplyPlan() expected error")
	}

	var de *deploy.DeployError
	if !errors.As(err, &de) {
		t.Fatalf("ApplyPlan() error = %T, want *deploy.DeployError", err)
	}
	if de.Phase != "postcondition" {
		t.Fatalf("DeployError.Phase = %q, want postcondition", de.Phase)
	}

	info, inspectErr := rt.ContainerInspect(context.Background(), "ployz-ns-api-a001")
	if inspectErr != nil {
		t.Fatalf("ContainerInspect() error = %v", inspectErr)
	}
	if !info.Exists {
		t.Fatalf("container exists after postcondition failure = %v, want true", info.Exists)
	}

	rows, listErr := containerStore.ListContainersByNamespace(context.Background(), "ns")
	if listErr != nil {
		t.Fatalf("ListContainersByNamespace() error = %v", listErr)
	}
	if len(rows) != 1 {
		t.Fatalf("container rows after postcondition failure = %+v, want one row", rows)
	}
}

func drainEventTypes(events <-chan deploy.ProgressEvent) []string {
	var out []string
	for {
		select {
		case ev := <-events:
			out = append(out, ev.Type)
		default:
			return out
		}
	}
}

func contains(items []string, target string) bool {
	for _, item := range items {
		if item == target {
			return true
		}
	}
	return false
}
