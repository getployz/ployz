package service

import (
	"context"
	"strings"
	"testing"

	"ployz/internal/deploy"
)

func TestBuildServiceComposeSpec(t *testing.T) {
	spec, err := buildServiceComposeSpec(
		"api",
		"ghcr.io/acme/api:v1",
		3,
		[]string{"80:8080"},
		[]string{"APP_ENV=prod", "EMPTY="},
	)
	if err != nil {
		t.Fatalf("buildServiceComposeSpec() error = %v", err)
	}

	project, err := deploy.LoadSpec(context.Background(), spec, "api")
	if err != nil {
		t.Fatalf("LoadSpec() error = %v", err)
	}

	svc, err := project.GetService("api")
	if err != nil {
		t.Fatalf("GetService() error = %v", err)
	}

	if svc.Image != "ghcr.io/acme/api:v1" {
		t.Fatalf("service image = %q, want ghcr.io/acme/api:v1", svc.Image)
	}
	if svc.Deploy == nil || svc.Deploy.Replicas == nil || *svc.Deploy.Replicas != 3 {
		t.Fatalf("service replicas = %v, want 3", svc.Deploy)
	}
	if len(svc.Ports) != 1 {
		t.Fatalf("port count = %d, want 1", len(svc.Ports))
	}
	if svc.Ports[0].Published != "80" {
		t.Fatalf("published port = %q, want 80", svc.Ports[0].Published)
	}
	if svc.Ports[0].Target != 8080 {
		t.Fatalf("target port = %d, want 8080", svc.Ports[0].Target)
	}

	if got := svc.Environment["APP_ENV"]; got == nil || *got != "prod" {
		t.Fatalf("APP_ENV = %v, want prod", got)
	}
	if got := svc.Environment["EMPTY"]; got == nil || *got != "" {
		t.Fatalf("EMPTY = %v, want empty string", got)
	}
}

func TestBuildServiceComposeSpecFailure(t *testing.T) {
	testCases := []struct {
		name     string
		image    string
		replicas int
		ports    []string
		envVars  []string
		wantErr  string
	}{
		{
			name:     "missing image",
			replicas: 1,
			wantErr:  "image is required",
		},
		{
			name:     "invalid replicas",
			image:    "nginx:latest",
			replicas: 0,
			wantErr:  "replicas must be at least 1",
		},
		{
			name:     "empty port entry",
			image:    "nginx:latest",
			replicas: 1,
			ports:    []string{""},
			wantErr:  "port entries must not be empty",
		},
		{
			name:     "invalid env entry",
			image:    "nginx:latest",
			replicas: 1,
			envVars:  []string{"BROKEN"},
			wantErr:  "must be KEY=VALUE",
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			_, err := buildServiceComposeSpec("api", tc.image, tc.replicas, tc.ports, tc.envVars)
			if err == nil {
				t.Fatalf("buildServiceComposeSpec() error = nil, want %q", tc.wantErr)
			}
			if !strings.Contains(err.Error(), tc.wantErr) {
				t.Fatalf("buildServiceComposeSpec() error = %q, want contains %q", err, tc.wantErr)
			}
		})
	}
}
