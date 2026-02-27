package sqlite

import (
	"context"
	"path/filepath"
	"testing"
	"time"

	"ployz/internal/observed"
)

func TestRuntimeContainerStoreReplaceAndListNamespace(t *testing.T) {
	t.Parallel()

	ctx := context.Background()
	store := RuntimeContainerStore{}
	dataDir := filepath.Join(t.TempDir(), "default")
	observedAt := time.Date(2026, 2, 26, 12, 0, 0, 0, time.UTC)

	rows := []observed.ContainerRecord{
		{
			MachineID:     "machine-1",
			Namespace:     "default",
			DeployID:      "deploy-1",
			ContainerName: "web-a",
			Image:         "nginx:1.27",
			Running:       true,
			Healthy:       true,
			Ports: []observed.ContainerPort{
				{HostIP: "127.0.0.1", HostPort: 8080, ContainerPort: 80, Protocol: "tcp"},
				{HostIP: "", HostPort: 8443, ContainerPort: 443, Protocol: "TCP"},
			},
		},
	}

	if err := store.ReplaceNamespaceSnapshot(ctx, dataDir, "machine-1", "default", rows, observedAt); err != nil {
		t.Fatalf("replace runtime container snapshot: %v", err)
	}

	got, err := store.ListNamespace(ctx, dataDir, "default")
	if err != nil {
		t.Fatalf("list runtime containers: %v", err)
	}
	if len(got) != 1 {
		t.Fatalf("expected 1 runtime container row, got %d", len(got))
	}
	if got[0].MachineID != "machine-1" {
		t.Fatalf("expected machine_id machine-1, got %q", got[0].MachineID)
	}
	if got[0].DeployID != "deploy-1" {
		t.Fatalf("expected deploy_id deploy-1, got %q", got[0].DeployID)
	}
	if got[0].ContainerName != "web-a" {
		t.Fatalf("expected container_name web-a, got %q", got[0].ContainerName)
	}
	if got[0].Image != "nginx:1.27" {
		t.Fatalf("expected image nginx:1.27, got %q", got[0].Image)
	}
	if !got[0].Running {
		t.Fatalf("expected running=true")
	}
	if !got[0].Healthy {
		t.Fatalf("expected healthy=true")
	}
	if got[0].ObservedAt != observedAt.Format(time.RFC3339Nano) {
		t.Fatalf("expected observed_at %q, got %q", observedAt.Format(time.RFC3339Nano), got[0].ObservedAt)
	}
	if len(got[0].Ports) != 2 {
		t.Fatalf("expected 2 runtime container ports, got %d", len(got[0].Ports))
	}
	if got[0].Ports[0].HostPort != 8080 || got[0].Ports[0].ContainerPort != 80 {
		t.Fatalf("expected first port 8080->80, got %d->%d", got[0].Ports[0].HostPort, got[0].Ports[0].ContainerPort)
	}
	if got[0].Ports[1].Protocol != "tcp" {
		t.Fatalf("expected normalized second protocol tcp, got %q", got[0].Ports[1].Protocol)
	}

	if err := store.ReplaceNamespaceSnapshot(ctx, dataDir, "machine-1", "default", nil, observedAt.Add(time.Minute)); err != nil {
		t.Fatalf("replace runtime container snapshot with empty set: %v", err)
	}

	got, err = store.ListNamespace(ctx, dataDir, "default")
	if err != nil {
		t.Fatalf("list runtime containers after delete: %v", err)
	}
	if len(got) != 0 {
		t.Fatalf("expected 0 runtime container rows after empty snapshot, got %d", len(got))
	}
}

func TestRuntimeContainerStoreValidationErrors(t *testing.T) {
	t.Parallel()

	ctx := context.Background()
	store := RuntimeContainerStore{}
	dataDir := filepath.Join(t.TempDir(), "default")

	if _, err := store.ListNamespace(ctx, "", "default"); err == nil {
		t.Fatalf("expected error for empty data dir")
	}
	if _, err := store.ListNamespace(ctx, dataDir, ""); err == nil {
		t.Fatalf("expected error for empty namespace")
	}

	if err := store.ReplaceNamespaceSnapshot(ctx, dataDir, "", "default", nil, time.Time{}); err == nil {
		t.Fatalf("expected error for empty machine id")
	}
	if err := store.ReplaceNamespaceSnapshot(ctx, dataDir, "machine-1", "", nil, time.Time{}); err == nil {
		t.Fatalf("expected error for empty namespace")
	}

	badRows := []observed.ContainerRecord{{ContainerName: ""}}
	if err := store.ReplaceNamespaceSnapshot(ctx, dataDir, "machine-1", "default", badRows, time.Time{}); err == nil {
		t.Fatalf("expected error for missing container name")
	}
}
