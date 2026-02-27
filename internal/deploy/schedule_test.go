package deploy

import (
	"errors"
	"testing"
)

func TestScheduleNoMachines(t *testing.T) {
	services := []ServiceDeployConfig{
		{
			Spec: ServiceSpec{Name: "web"},
		},
	}

	_, err := Schedule("default", services, nil, nil)
	if !errors.Is(err, ErrNoMachinesAvailable) {
		t.Fatalf("expected ErrNoMachinesAvailable, got %v", err)
	}
}

func TestScheduleReplicatedAssignsMachine(t *testing.T) {
	services := []ServiceDeployConfig{
		{
			Spec:      ServiceSpec{Name: "web"},
			Placement: PlacementReplicated,
			Replicas:  1,
		},
	}
	machines := []MachineInfo{{ID: "m1", Labels: map[string]string{}}}

	assignments, err := Schedule("default", services, machines, nil)
	if err != nil {
		t.Fatalf("schedule failed: %v", err)
	}
	web := assignments["web"]
	if len(web) != 1 {
		t.Fatalf("expected 1 assignment, got %d", len(web))
	}
	if web[0].MachineID != "m1" {
		t.Fatalf("expected machine m1, got %q", web[0].MachineID)
	}
}
