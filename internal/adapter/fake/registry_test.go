package fake

import (
	"context"
	"errors"
	"net/netip"
	"testing"
	"time"

	"ployz/internal/mesh"
)

func mustPrefix(s string) netip.Prefix {
	return netip.MustParsePrefix(s)
}

func TestRegistry_CallRecording(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)
	reg := c.Registry("node-a")

	ctx := context.Background()
	_ = reg.EnsureMachineTable(ctx)
	_ = reg.EnsureNetworkConfigTable(ctx)
	_ = reg.EnsureHeartbeatTable(ctx)

	row := mesh.MachineRow{ID: "m1", PublicKey: "pk1"}
	_ = reg.UpsertMachine(ctx, row, 0)
	_, _ = reg.ListMachineRows(ctx)

	if len(reg.Calls("EnsureMachineTable")) != 1 {
		t.Error("expected 1 EnsureMachineTable call")
	}
	if len(reg.Calls("UpsertMachine")) != 1 {
		t.Error("expected 1 UpsertMachine call")
	}
	if len(reg.Calls("ListMachineRows")) != 1 {
		t.Error("expected 1 ListMachineRows call")
	}
}

func TestRegistry_ErrorInjection(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)
	reg := c.Registry("node-a")

	ctx := context.Background()
	injected := errors.New("network unreachable")

	reg.UpsertMachineErr = func(context.Context, mesh.MachineRow, int64) error {
		return injected
	}
	err := reg.UpsertMachine(ctx, mesh.MachineRow{ID: "m1"}, 0)
	if !errors.Is(err, injected) {
		t.Errorf("expected injected error, got %v", err)
	}

	// Verify the failed upsert didn't modify state.
	rows, _ := reg.ListMachineRows(ctx)
	if len(rows) != 0 {
		t.Error("expected no machines after failed upsert")
	}
}

func TestRegistry_FaultFailOnce(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)
	reg := c.Registry("node-a")

	ctx := context.Background()
	injected := errors.New("fail once")
	reg.FailOnce(FaultRegistryUpsertMachine, injected)

	err := reg.UpsertMachine(ctx, mesh.MachineRow{ID: "m1", PublicKey: "pk1"}, 0)
	if !errors.Is(err, injected) {
		t.Fatalf("first UpsertMachine error = %v, want %v", err, injected)
	}

	err = reg.UpsertMachine(ctx, mesh.MachineRow{ID: "m1", PublicKey: "pk1"}, 0)
	if err != nil {
		t.Fatalf("second UpsertMachine error = %v, want nil", err)
	}
}

func TestRegistry_FaultHook(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)
	reg := c.Registry("node-a")

	ctx := context.Background()
	injected := errors.New("bad machine id")
	reg.SetFaultHook(FaultRegistryUpsertMachine, func(args ...any) error {
		if len(args) < 2 {
			return nil
		}
		row, _ := args[1].(mesh.MachineRow)
		if row.ID == "bad" {
			return injected
		}
		return nil
	})

	err := reg.UpsertMachine(ctx, mesh.MachineRow{ID: "bad", PublicKey: "pk-bad"}, 0)
	if !errors.Is(err, injected) {
		t.Fatalf("bad machine upsert error = %v, want %v", err, injected)
	}

	err = reg.UpsertMachine(ctx, mesh.MachineRow{ID: "ok", PublicKey: "pk-ok"}, 0)
	if err != nil {
		t.Fatalf("ok machine upsert error = %v, want nil", err)
	}
}

func TestRegistry_SubscribeMachinesError(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)
	reg := c.Registry("node-a")

	injected := errors.New("subscribe failed")
	reg.SubscribeMachinesErr = func(context.Context) error { return injected }

	_, _, err := reg.SubscribeMachines(context.Background())
	if !errors.Is(err, injected) {
		t.Errorf("expected injected error, got %v", err)
	}
}

func TestRegistry_DeleteByEndpointExceptID(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)
	reg := c.Registry("node-a")

	ctx := context.Background()
	_ = reg.UpsertMachine(ctx, mesh.MachineRow{ID: "m1", Endpoint: "1.2.3.4:51820"}, 0)
	_ = reg.UpsertMachine(ctx, mesh.MachineRow{ID: "m2", Endpoint: "1.2.3.4:51820"}, 0)
	_ = reg.UpsertMachine(ctx, mesh.MachineRow{ID: "m3", Endpoint: "5.6.7.8:51820"}, 0)

	// Delete all with endpoint 1.2.3.4:51820 except m1.
	if err := reg.DeleteByEndpointExceptID(ctx, "1.2.3.4:51820", "m1"); err != nil {
		t.Fatal(err)
	}

	rows, _ := reg.ListMachineRows(ctx)
	if len(rows) != 2 {
		t.Fatalf("expected 2 remaining machines, got %d", len(rows))
	}

	ids := make(map[string]bool)
	for _, r := range rows {
		ids[r.ID] = true
	}
	if !ids["m1"] || !ids["m3"] {
		t.Errorf("expected m1 and m3, got %v", ids)
	}
}

func TestRegistry_BumpHeartbeat(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)
	reg := c.Registry("node-a")

	ctx := context.Background()
	if err := reg.BumpHeartbeat(ctx, "node-a", "2025-01-01T00:00:00Z"); err != nil {
		t.Fatal(err)
	}

	snap := c.Snapshot("node-a")
	if len(snap.Heartbeats) != 1 {
		t.Fatalf("expected 1 heartbeat, got %d", len(snap.Heartbeats))
	}
	if snap.Heartbeats[0].Seq != 1 {
		t.Errorf("expected seq 1, got %d", snap.Heartbeats[0].Seq)
	}

	// Bump again.
	_ = reg.BumpHeartbeat(ctx, "node-a", "2025-01-01T00:01:00Z")
	snap = c.Snapshot("node-a")
	if snap.Heartbeats[0].Seq != 2 {
		t.Errorf("expected seq 2, got %d", snap.Heartbeats[0].Seq)
	}
}
