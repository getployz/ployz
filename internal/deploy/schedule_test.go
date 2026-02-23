package deploy

import (
	"regexp"
	"testing"
)

func TestParseConstraint(t *testing.T) {
	tests := []struct {
		name        string
		constraint  string
		wantKey     string
		wantOp      string
		wantValue   string
		expectError bool
	}{
		{name: "equals", constraint: "node.labels.gpu == true", wantKey: "gpu", wantOp: "==", wantValue: "true"},
		{name: "not equals", constraint: "node.labels.region != us-east", wantKey: "region", wantOp: "!=", wantValue: "us-east"},
		{name: "invalid prefix", constraint: "service.labels.gpu == true", expectError: true},
		{name: "invalid operator", constraint: "node.labels.gpu > true", expectError: true},
		{name: "empty", constraint: "", expectError: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			key, op, value, err := ParseConstraint(tt.constraint)
			if tt.expectError {
				if err == nil {
					t.Fatalf("ParseConstraint(%q) expected error", tt.constraint)
				}
				return
			}
			if err != nil {
				t.Fatalf("ParseConstraint(%q) error = %v", tt.constraint, err)
			}
			if key != tt.wantKey || op != tt.wantOp || value != tt.wantValue {
				t.Fatalf("ParseConstraint(%q) = (%q, %q, %q), want (%q, %q, %q)", tt.constraint, key, op, value, tt.wantKey, tt.wantOp, tt.wantValue)
			}
		})
	}
}

func TestMatchConstraints(t *testing.T) {
	machine := MachineInfo{ID: "a", Labels: map[string]string{"gpu": "true", "region": "us-east"}}

	tests := []struct {
		name        string
		constraints []string
		want        bool
	}{
		{name: "no constraints", constraints: nil, want: true},
		{name: "equals match", constraints: []string{"node.labels.gpu == true"}, want: true},
		{name: "equals missing label", constraints: []string{"node.labels.zone == a"}, want: false},
		{name: "not equals match", constraints: []string{"node.labels.gpu != false"}, want: true},
		{name: "not equals fail", constraints: []string{"node.labels.gpu != true"}, want: false},
		{name: "multiple all match", constraints: []string{"node.labels.gpu == true", "node.labels.region == us-east"}, want: true},
		{name: "multiple one fails", constraints: []string{"node.labels.gpu == true", "node.labels.region == us-west"}, want: false},
		{name: "unknown format", constraints: []string{"bad-constraint-format"}, want: false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := MatchConstraints(tt.constraints, machine)
			if got != tt.want {
				t.Fatalf("MatchConstraints(%v) = %v, want %v", tt.constraints, got, tt.want)
			}
		})
	}
}

func TestSchedule_GlobalPlacement(t *testing.T) {
	machines := fixtureMachines()
	services := []ServiceDeployConfig{
		newService("global", PlacementGlobal, 0, nil, nil),
	}

	assignments, err := Schedule("ns", services, machines, nil)
	if err != nil {
		t.Fatalf("Schedule() error = %v", err)
	}
	got := assignments["global"]
	if len(got) != 3 {
		t.Fatalf("global assignments = %d, want 3", len(got))
	}
	assertUniqueMachineIDs(t, got)
	assertContainerNameFormat(t, got)
}

func TestSchedule_GlobalPlacementWithConstraints(t *testing.T) {
	machines := fixtureMachines()
	services := []ServiceDeployConfig{
		newService("global", PlacementGlobal, 0, []string{"node.labels.region == us-east"}, nil),
	}

	assignments, err := Schedule("ns", services, machines, nil)
	if err != nil {
		t.Fatalf("Schedule() error = %v", err)
	}
	got := assignments["global"]
	if len(got) != 2 {
		t.Fatalf("global constrained assignments = %d, want 2", len(got))
	}
}

func TestSchedule_NoEligibleMachines(t *testing.T) {
	machines := fixtureMachines()
	services := []ServiceDeployConfig{
		newService("gpu", PlacementReplicated, 1, []string{"node.labels.gpu == maybe"}, nil),
	}

	_, err := Schedule("ns", services, machines, nil)
	if err == nil {
		t.Fatal("Schedule() expected error for zero eligible machines")
	}
}

func TestSchedule_ReplicatedPlacement(t *testing.T) {
	machines := fixtureMachines()

	t.Run("three replicas on three machines", func(t *testing.T) {
		services := []ServiceDeployConfig{newService("api", PlacementReplicated, 3, nil, nil)}
		assignments, err := Schedule("ns", services, machines, nil)
		if err != nil {
			t.Fatalf("Schedule() error = %v", err)
		}
		got := assignments["api"]
		if len(got) != 3 {
			t.Fatalf("assignments = %d, want 3", len(got))
		}
		assertUniqueMachineIDs(t, got)
	})

	t.Run("two replicas on three machines", func(t *testing.T) {
		services := []ServiceDeployConfig{newService("api", PlacementReplicated, 2, nil, nil)}
		assignments, err := Schedule("ns", services, machines, nil)
		if err != nil {
			t.Fatalf("Schedule() error = %v", err)
		}
		got := assignments["api"]
		if len(got) != 2 {
			t.Fatalf("assignments = %d, want 2", len(got))
		}
		assertUniqueMachineIDs(t, got)
	})

	t.Run("five replicas wraps over three machines", func(t *testing.T) {
		services := []ServiceDeployConfig{newService("api", PlacementReplicated, 5, nil, nil)}
		assignments, err := Schedule("ns", services, machines, nil)
		if err != nil {
			t.Fatalf("Schedule() error = %v", err)
		}
		got := assignments["api"]
		if len(got) != 5 {
			t.Fatalf("assignments = %d, want 5", len(got))
		}
		counts := countByMachine(got)
		if len(counts) != 3 {
			t.Fatalf("machines used = %d, want 3", len(counts))
		}
	})

	t.Run("zero replicas defaults to one", func(t *testing.T) {
		services := []ServiceDeployConfig{newService("api", PlacementReplicated, 0, nil, nil)}
		assignments, err := Schedule("ns", services, machines, nil)
		if err != nil {
			t.Fatalf("Schedule() error = %v", err)
		}
		if len(assignments["api"]) != 1 {
			t.Fatalf("assignments = %d, want 1", len(assignments["api"]))
		}
	})

	t.Run("constraint to one machine", func(t *testing.T) {
		services := []ServiceDeployConfig{newService("gpu", PlacementReplicated, 3, []string{"node.labels.gpu == true"}, nil)}
		assignments, err := Schedule("ns", services, machines, nil)
		if err != nil {
			t.Fatalf("Schedule() error = %v", err)
		}
		got := assignments["gpu"]
		if len(got) != 3 {
			t.Fatalf("assignments = %d, want 3", len(got))
		}
		for _, a := range got {
			if a.MachineID != "a" {
				t.Fatalf("expected all replicas on machine a, got %q", a.MachineID)
			}
		}
	})
}

func TestSchedule_AntiAffinity(t *testing.T) {
	machines := fixtureMachines()
	current := []ContainerRow{
		{Service: "api", MachineID: "a"},
	}
	services := []ServiceDeployConfig{newService("api", PlacementReplicated, 2, []string{"node.labels.region != us-west"}, nil)}

	assignments, err := Schedule("ns", services, machines, current)
	if err != nil {
		t.Fatalf("Schedule() error = %v", err)
	}
	got := assignments["api"]
	if len(got) != 2 {
		t.Fatalf("assignments = %d, want 2", len(got))
	}
	if got[0].MachineID != "c" {
		t.Fatalf("first replica machine = %q, want %q", got[0].MachineID, "c")
	}
}

func TestSchedule_VolumeAffinity(t *testing.T) {
	machines := fixtureMachines()

	t.Run("shared named volume co-locates", func(t *testing.T) {
		sharedMount := []Mount{{Source: "data", Target: "/data"}}
		services := []ServiceDeployConfig{
			newService("db", PlacementReplicated, 1, nil, sharedMount),
			newService("api", PlacementReplicated, 1, nil, sharedMount),
		}

		assignments, err := Schedule("ns", services, machines, nil)
		if err != nil {
			t.Fatalf("Schedule() error = %v", err)
		}
		dbMachine := assignments["db"][0].MachineID
		apiMachine := assignments["api"][0].MachineID
		if dbMachine != apiMachine {
			t.Fatalf("expected shared-volume services on same machine, got db=%q api=%q", dbMachine, apiMachine)
		}
	})

	t.Run("mismatched replica counts fail", func(t *testing.T) {
		sharedMount := []Mount{{Source: "data", Target: "/data"}}
		services := []ServiceDeployConfig{
			newService("db", PlacementReplicated, 3, nil, sharedMount),
			newService("api", PlacementReplicated, 2, nil, sharedMount),
		}

		_, err := Schedule("ns", services, machines, nil)
		if err == nil {
			t.Fatal("Schedule() expected error for mismatched shared-volume replicas")
		}
	})

	t.Run("independent services without shared volume", func(t *testing.T) {
		services := []ServiceDeployConfig{
			newService("db", PlacementReplicated, 1, nil, []Mount{{Source: "db-data", Target: "/data"}}),
			newService("api", PlacementReplicated, 1, nil, []Mount{{Source: "api-cache", Target: "/cache"}}),
		}

		assignments, err := Schedule("ns", services, machines, nil)
		if err != nil {
			t.Fatalf("Schedule() error = %v", err)
		}
		if len(assignments["db"]) != 1 || len(assignments["api"]) != 1 {
			t.Fatalf("expected one assignment each, got db=%d api=%d", len(assignments["db"]), len(assignments["api"]))
		}
	})
}

func TestSchedule_MixedConstraintsAndVolumeGroup(t *testing.T) {
	machines := fixtureMachines()
	sharedMount := []Mount{{Source: "data", Target: "/data"}}
	services := []ServiceDeployConfig{
		newService("db", PlacementReplicated, 1, []string{"node.labels.region == us-east"}, sharedMount),
		newService("api", PlacementReplicated, 1, []string{"node.labels.region == eu-central"}, sharedMount),
	}

	_, err := Schedule("ns", services, machines, nil)
	if err == nil {
		t.Fatal("Schedule() expected error when volume-group constraints eliminate all machines")
	}
}

func newService(name string, placement PlacementMode, replicas int, constraints []string, mounts []Mount) ServiceDeployConfig {
	return ServiceDeployConfig{
		Spec: ServiceSpec{
			Name:   name,
			Image:  "nginx:latest",
			Mounts: mounts,
		},
		Placement:   placement,
		Replicas:    replicas,
		Constraints: constraints,
	}
}

func fixtureMachines() []MachineInfo {
	return []MachineInfo{
		{ID: "a", Labels: map[string]string{"region": "us-east", "gpu": "true"}},
		{ID: "b", Labels: map[string]string{"region": "us-west", "gpu": "false"}},
		{ID: "c", Labels: map[string]string{"region": "us-east", "gpu": "false"}},
	}
}

func assertUniqueMachineIDs(t *testing.T, assignments []MachineAssignment) {
	t.Helper()
	seen := make(map[string]bool, len(assignments))
	for _, a := range assignments {
		if seen[a.MachineID] {
			t.Fatalf("duplicate machine assignment for %q", a.MachineID)
		}
		seen[a.MachineID] = true
	}
}

func countByMachine(assignments []MachineAssignment) map[string]int {
	out := make(map[string]int, len(assignments))
	for _, a := range assignments {
		out[a.MachineID]++
	}
	return out
}

func assertContainerNameFormat(t *testing.T, assignments []MachineAssignment) {
	t.Helper()
	re := regexp.MustCompile(`^ployz-ns-[a-z0-9-]+-[0-9a-f]{4}$`)
	for _, a := range assignments {
		if !re.MatchString(a.ContainerName) {
			t.Fatalf("container name %q does not match %q", a.ContainerName, re.String())
		}
	}
}
