package deploy_test

import (
	"testing"

	"ployz/internal/deploy"
)

func TestAssertTierState_AllMatch(t *testing.T) {
	actual := []deploy.ContainerState{
		{ContainerName: "api-1", Image: "api:1", Running: true},
	}
	expected := []deploy.ContainerResult{
		{MachineID: "m1", ContainerName: "api-1", Expected: "api:1"},
	}

	if err := deploy.AssertTierState(actual, expected); err != nil {
		t.Fatalf("AssertTierState() error = %v", err)
	}
}

func TestAssertTierState_Mismatch(t *testing.T) {
	actual := []deploy.ContainerState{
		{ContainerName: "api-1", Image: "api:2", Running: true},
	}
	expected := []deploy.ContainerResult{
		{MachineID: "m1", ContainerName: "api-1", Expected: "api:1"},
		{MachineID: "m1", ContainerName: "api-2", Expected: "api:1"},
	}

	err := deploy.AssertTierState(actual, expected)
	if err == nil {
		t.Fatal("AssertTierState() expected error")
	}
	if err.Phase != "postcondition" {
		t.Fatalf("DeployError.Phase = %q, want postcondition", err.Phase)
	}
	if len(err.Tiers) != 1 || len(err.Tiers[0].Containers) != 2 {
		t.Fatalf("DeployError.Tiers = %+v, want one tier with two container rows", err.Tiers)
	}
	if err.Tiers[0].Containers[0].Match {
		t.Fatalf("first row Match = true, want false: %+v", err.Tiers[0].Containers[0])
	}
	if err.Tiers[0].Containers[1].Actual != "missing" {
		t.Fatalf("second row Actual = %q, want missing", err.Tiers[0].Containers[1].Actual)
	}
}
