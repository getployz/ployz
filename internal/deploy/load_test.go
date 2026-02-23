package deploy

import (
	"context"
	"strings"
	"testing"

	compose "github.com/compose-spec/compose-go/v2/types"
)

func TestLoadSpec_ValidCompose(t *testing.T) {
	ctx := context.Background()
	spec := []byte(`
name: app
services:
  web:
    image: nginx:1.25
  api:
    image: ghcr.io/example/api:latest
`)

	project, err := LoadSpec(ctx, spec, "")
	if err != nil {
		t.Fatalf("LoadSpec() error = %v", err)
	}
	if project.Name != "app" {
		t.Fatalf("project.Name = %q, want %q", project.Name, "app")
	}
	if len(project.Services) != 2 {
		t.Fatalf("len(project.Services) = %d, want 2", len(project.Services))
	}

	web := serviceByName(t, project, "web")
	if web.Image != "nginx:1.25" {
		t.Fatalf("web image = %q, want %q", web.Image, "nginx:1.25")
	}
	api := serviceByName(t, project, "api")
	if api.Image != "ghcr.io/example/api:latest" {
		t.Fatalf("api image = %q, want %q", api.Image, "ghcr.io/example/api:latest")
	}
}

func TestLoadSpec_NamespaceOverride(t *testing.T) {
	ctx := context.Background()
	spec := []byte(`
name: from-compose
services:
  web:
    image: nginx:1.25
`)

	project, err := LoadSpec(ctx, spec, "override")
	if err != nil {
		t.Fatalf("LoadSpec() error = %v", err)
	}
	if project.Name != "override" {
		t.Fatalf("project.Name = %q, want %q", project.Name, "override")
	}
}

func TestLoadSpec_FeaturesParsed(t *testing.T) {
	ctx := context.Background()
	spec := []byte(`
name: feature-app
services:
  db:
    image: postgres:16
  app:
    image: ghcr.io/example/app:latest
    depends_on:
      db:
        condition: service_started
    deploy:
      mode: replicated
      replicas: 3
      labels:
        deploy.role: api
      placement:
        constraints:
          - node.labels.region == us-east
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost/health"]
      interval: 5s
      timeout: 2s
      retries: 3
    x-ployz:
      tier: 1
`)

	project, err := LoadSpec(ctx, spec, "")
	if err != nil {
		t.Fatalf("LoadSpec() error = %v", err)
	}

	app := serviceByName(t, project, "app")
	if _, ok := app.DependsOn["db"]; !ok {
		t.Fatalf("expected app to depend on db, depends_on=%v", app.DependsOn)
	}
	if app.Deploy == nil {
		t.Fatal("expected app deploy config to be populated")
	}
	if app.Deploy.Mode != "replicated" {
		t.Fatalf("deploy.mode = %q, want %q", app.Deploy.Mode, "replicated")
	}
	if app.Deploy.Replicas == nil || *app.Deploy.Replicas != 3 {
		t.Fatalf("deploy.replicas = %v, want 3", app.Deploy.Replicas)
	}
	if app.Deploy.Labels["deploy.role"] != "api" {
		t.Fatalf("deploy.labels[deploy.role] = %q, want %q", app.Deploy.Labels["deploy.role"], "api")
	}
	if len(app.Deploy.Placement.Constraints) != 1 || app.Deploy.Placement.Constraints[0] != "node.labels.region == us-east" {
		t.Fatalf("placement.constraints = %v, want [node.labels.region == us-east]", app.Deploy.Placement.Constraints)
	}
	if app.HealthCheck == nil {
		t.Fatal("expected app healthcheck to be populated")
	}
	if len(app.HealthCheck.Test) != 4 {
		t.Fatalf("healthcheck.test length = %d, want 4", len(app.HealthCheck.Test))
	}
	if _, ok := app.Extensions["x-ployz"]; !ok {
		t.Fatalf("expected x-ployz extension to be present, extensions=%v", app.Extensions)
	}
}

func TestLoadSpec_InvalidCompose(t *testing.T) {
	ctx := context.Background()
	tests := []struct {
		name string
		spec []byte
	}{
		{
			name: "malformed yaml",
			spec: []byte(`
services:
  web:
    image: nginx:1.25
      bad-indent: true
`),
		},
		{
			name: "no services",
			spec: []byte(`
name: empty
`),
		},
		{
			name: "missing image",
			spec: []byte(`
services:
  web: {}
`),
		},
		{
			name: "empty yaml",
			spec: []byte(""),
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := LoadSpec(ctx, tt.spec, "")
			if err == nil {
				t.Fatal("LoadSpec() expected error, got nil")
			}
			if !strings.Contains(err.Error(), "compose") && !strings.Contains(err.Error(), "parse") {
				t.Fatalf("LoadSpec() error = %v, expected parse/compose context", err)
			}
		})
	}
}

func serviceByName(t *testing.T, project *compose.Project, name string) compose.ServiceConfig {
	t.Helper()
	for _, svc := range project.Services {
		if svc.Name == name {
			return svc
		}
	}
	t.Fatalf("service %q not found", name)
	return compose.ServiceConfig{}
}
