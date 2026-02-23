package deploy

import (
	"testing"
	"time"

	compose "github.com/compose-spec/compose-go/v2/types"
)

func TestNormalizeServiceSpec(t *testing.T) {
	t.Run("basic service", func(t *testing.T) {
		svc := compose.ServiceConfig{
			Name:  "web",
			Image: "nginx:1.25",
		}

		got := NormalizeServiceSpec(svc)
		if got.Name != "web" {
			t.Fatalf("Name = %q, want %q", got.Name, "web")
		}
		if got.Image != "nginx:1.25" {
			t.Fatalf("Image = %q, want %q", got.Image, "nginx:1.25")
		}
		if got.Command != nil || got.Entrypoint != nil || got.Environment != nil || got.Mounts != nil || got.Ports != nil || got.Labels != nil || got.HealthCheck != nil || got.Resources != nil {
			t.Fatalf("expected zero value fields, got %+v", got)
		}
	})

	t.Run("full service normalization", func(t *testing.T) {
		interval := compose.Duration(5 * time.Second)
		timeout := compose.Duration(2 * time.Second)
		startPeriod := compose.Duration(1 * time.Second)
		retries := uint64(3)
		envA := "1"
		envB := "2"

		svc := compose.ServiceConfig{
			Name:       "app",
			Image:      "ghcr.io/example/app:latest",
			Command:    compose.ShellCommand{"run", "server"},
			Entrypoint: compose.ShellCommand{"/entrypoint.sh"},
			Environment: compose.MappingWithEquals{
				"B": &envB,
				"A": &envA,
			},
			Volumes: []compose.ServiceVolumeConfig{
				{Type: "bind", Source: "/var/lib/data", Target: "/data", ReadOnly: true},
				{Type: "volume", Source: "cache", Target: "/cache"},
			},
			Ports: []compose.ServicePortConfig{
				{Published: "8443", Target: 443, Protocol: "tcp"},
				{Published: "8080", Target: 80},
			},
			Labels:  compose.Labels{"z": "9", "a": "1"},
			Restart: "unless-stopped",
			HealthCheck: &compose.HealthCheckConfig{
				Test:        compose.HealthCheckTest{"CMD", "curl", "-f", "http://localhost/health"},
				Interval:    &interval,
				Timeout:     &timeout,
				Retries:     &retries,
				StartPeriod: &startPeriod,
			},
			Deploy: &compose.DeployConfig{
				Resources: compose.Resources{
					Limits: &compose.Resource{
						NanoCPUs:    compose.NanoCPUs(2),
						MemoryBytes: compose.UnitBytes(128 * 1024 * 1024),
					},
				},
			},
		}

		got := NormalizeServiceSpec(svc)
		want := ServiceSpec{
			Name:       "app",
			Image:      "ghcr.io/example/app:latest",
			Command:    []string{"run", "server"},
			Entrypoint: []string{"/entrypoint.sh"},
			Environment: []string{
				"A=1",
				"B=2",
			},
			Mounts: []Mount{
				{Source: "/var/lib/data", Target: "/data", ReadOnly: true},
				{Source: "cache", Target: "/cache", ReadOnly: false},
			},
			Ports: []PortMapping{
				{HostPort: 8080, ContainerPort: 80, Protocol: "tcp"},
				{HostPort: 8443, ContainerPort: 443, Protocol: "tcp"},
			},
			Labels:        map[string]string{"a": "1", "z": "9"},
			RestartPolicy: "unless-stopped",
			HealthCheck: &HealthCheck{
				Test:        []string{"CMD", "curl", "-f", "http://localhost/health"},
				Interval:    5 * time.Second,
				Timeout:     2 * time.Second,
				Retries:     3,
				StartPeriod: 1 * time.Second,
			},
			Resources: &Resources{CPULimit: 2, MemoryLimit: 128 * 1024 * 1024},
		}
		if !SpecEqual(got, want) {
			t.Fatalf("NormalizeServiceSpec() mismatch:\n got: %+v\nwant: %+v", got, want)
		}
	})

	t.Run("healthcheck disabled", func(t *testing.T) {
		svc := compose.ServiceConfig{
			Name:  "api",
			Image: "ghcr.io/example/api:latest",
			HealthCheck: &compose.HealthCheckConfig{
				Disable: true,
			},
		}

		got := NormalizeServiceSpec(svc)
		if got.HealthCheck != nil {
			t.Fatalf("HealthCheck = %+v, want nil", got.HealthCheck)
		}
	})

	t.Run("empty fields normalize to zero values", func(t *testing.T) {
		svc := compose.ServiceConfig{
			Name:        "worker",
			Image:       "busybox:latest",
			Environment: compose.MappingWithEquals{},
			Labels:      compose.Labels{},
			Volumes:     []compose.ServiceVolumeConfig{},
			Ports:       []compose.ServicePortConfig{},
		}

		got := NormalizeServiceSpec(svc)
		if got.Environment != nil {
			t.Fatalf("Environment = %v, want nil", got.Environment)
		}
		if got.Labels != nil {
			t.Fatalf("Labels = %v, want nil", got.Labels)
		}
		if got.Mounts != nil {
			t.Fatalf("Mounts = %v, want nil", got.Mounts)
		}
		if got.Ports != nil {
			t.Fatalf("Ports = %v, want nil", got.Ports)
		}
	})
}

func TestClassifyChange(t *testing.T) {
	base := fixtureServiceSpec()

	tests := []struct {
		name     string
		mutate   func(spec *ServiceSpec)
		expected ChangeKind
	}{
		{
			name:     "identical specs",
			mutate:   func(_ *ServiceSpec) {},
			expected: UpToDate,
		},
		{
			name: "only cpu changed",
			mutate: func(spec *ServiceSpec) {
				spec.Resources.CPULimit = 2
			},
			expected: NeedsUpdate,
		},
		{
			name: "only memory changed",
			mutate: func(spec *ServiceSpec) {
				spec.Resources.MemoryLimit = 512 * 1024 * 1024
			},
			expected: NeedsUpdate,
		},
		{
			name: "cpu and memory changed",
			mutate: func(spec *ServiceSpec) {
				spec.Resources.CPULimit = 3
				spec.Resources.MemoryLimit = 1024 * 1024 * 1024
			},
			expected: NeedsUpdate,
		},
		{
			name: "image changed",
			mutate: func(spec *ServiceSpec) {
				spec.Image = "nginx:1.26"
			},
			expected: NeedsRecreate,
		},
		{
			name: "command changed",
			mutate: func(spec *ServiceSpec) {
				spec.Command = []string{"nginx", "-g", "daemon off;", "--verbose"}
			},
			expected: NeedsRecreate,
		},
		{
			name: "environment changed",
			mutate: func(spec *ServiceSpec) {
				spec.Environment = append(spec.Environment, "C=3")
			},
			expected: NeedsRecreate,
		},
		{
			name: "ports changed",
			mutate: func(spec *ServiceSpec) {
				spec.Ports[0].HostPort = 9090
			},
			expected: NeedsRecreate,
		},
		{
			name: "mounts changed",
			mutate: func(spec *ServiceSpec) {
				spec.Mounts = append(spec.Mounts, Mount{Source: "cache", Target: "/cache"})
			},
			expected: NeedsRecreate,
		},
		{
			name: "healthcheck added",
			mutate: func(spec *ServiceSpec) {
				spec.HealthCheck = &HealthCheck{Test: []string{"CMD", "true"}}
			},
			expected: NeedsRecreate,
		},
		{
			name: "healthcheck removed",
			mutate: func(spec *ServiceSpec) {
				spec.HealthCheck = nil
			},
			expected: NeedsRecreate,
		},
		{
			name: "labels changed",
			mutate: func(spec *ServiceSpec) {
				spec.Labels["tier"] = "api"
			},
			expected: NeedsRecreate,
		},
		{
			name: "restart policy changed",
			mutate: func(spec *ServiceSpec) {
				spec.RestartPolicy = "on-failure"
			},
			expected: NeedsRecreate,
		},
		{
			name: "resources and image changed",
			mutate: func(spec *ServiceSpec) {
				spec.Resources.CPULimit = 2
				spec.Image = "nginx:1.26"
			},
			expected: NeedsRecreate,
		},
		{
			name: "nil versus empty slices and map",
			mutate: func(spec *ServiceSpec) {
				spec.Entrypoint = []string{}
			},
			expected: UpToDate,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			incoming := cloneServiceSpec(base)
			tt.mutate(&incoming)
			got := ClassifyChange(base, incoming)
			if got != tt.expected {
				t.Fatalf("ClassifyChange() = %v, want %v", got, tt.expected)
			}
		})
	}
}

func TestSpecEqual(t *testing.T) {
	base := fixtureServiceSpec()

	t.Run("identical", func(t *testing.T) {
		if !SpecEqual(base, cloneServiceSpec(base)) {
			t.Fatal("SpecEqual() = false, want true")
		}
	})

	t.Run("different image", func(t *testing.T) {
		other := cloneServiceSpec(base)
		other.Image = "nginx:latest"
		if SpecEqual(base, other) {
			t.Fatal("SpecEqual() = true, want false")
		}
	})

	t.Run("nil versus empty slice", func(t *testing.T) {
		a := cloneServiceSpec(base)
		b := cloneServiceSpec(base)
		a.Command = nil
		b.Command = []string{}
		if !SpecEqual(a, b) {
			t.Fatal("SpecEqual() = false for nil/empty slice, want true")
		}
	})

	t.Run("nil versus empty map", func(t *testing.T) {
		a := cloneServiceSpec(base)
		b := cloneServiceSpec(base)
		a.Labels = nil
		b.Labels = map[string]string{}
		if !SpecEqual(a, b) {
			t.Fatal("SpecEqual() = false for nil/empty map, want true")
		}
	})
}

func fixtureServiceSpec() ServiceSpec {
	return ServiceSpec{
		Name:          "app",
		Image:         "nginx:1.25",
		Command:       []string{"nginx", "-g", "daemon off;"},
		Entrypoint:    nil,
		Environment:   []string{"A=1", "B=2"},
		Mounts:        []Mount{{Source: "/host/data", Target: "/data", ReadOnly: true}},
		Ports:         []PortMapping{{HostPort: 8080, ContainerPort: 80, Protocol: "tcp"}},
		Labels:        map[string]string{"app": "web"},
		RestartPolicy: "always",
		HealthCheck: &HealthCheck{
			Test:        []string{"CMD", "curl", "-f", "http://localhost/health"},
			Interval:    5 * time.Second,
			Timeout:     2 * time.Second,
			Retries:     3,
			StartPeriod: time.Second,
		},
		Resources: &Resources{
			CPULimit:    1,
			MemoryLimit: 256 * 1024 * 1024,
		},
	}
}

func cloneServiceSpec(in ServiceSpec) ServiceSpec {
	out := in
	out.Command = append([]string(nil), in.Command...)
	out.Entrypoint = append([]string(nil), in.Entrypoint...)
	out.Environment = append([]string(nil), in.Environment...)
	out.Mounts = append([]Mount(nil), in.Mounts...)
	out.Ports = append([]PortMapping(nil), in.Ports...)
	if in.Labels != nil {
		out.Labels = make(map[string]string, len(in.Labels))
		for key, value := range in.Labels {
			out.Labels[key] = value
		}
	}
	if in.HealthCheck != nil {
		out.HealthCheck = &HealthCheck{
			Test:        append([]string(nil), in.HealthCheck.Test...),
			Interval:    in.HealthCheck.Interval,
			Timeout:     in.HealthCheck.Timeout,
			Retries:     in.HealthCheck.Retries,
			StartPeriod: in.HealthCheck.StartPeriod,
		}
	}
	if in.Resources != nil {
		out.Resources = &Resources{CPULimit: in.Resources.CPULimit, MemoryLimit: in.Resources.MemoryLimit}
	}
	return out
}
