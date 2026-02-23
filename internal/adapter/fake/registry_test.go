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

	ctx := t.Context()
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

	ctx := t.Context()
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

	ctx := t.Context()
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

	ctx := t.Context()
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

	_, _, err := reg.SubscribeMachines(t.Context())
	if !errors.Is(err, injected) {
		t.Errorf("expected injected error, got %v", err)
	}
}

func TestRegistry_DeleteByEndpointExceptID(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)
	reg := c.Registry("node-a")

	ctx := t.Context()
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

	ctx := t.Context()
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

func TestRegistry_FaultPoints(t *testing.T) {
	tests := []struct {
		name  string
		point string
		setup func(*testing.T, *Registry)
		run   func(*testing.T, *Registry) error
	}{
		{
			name:  "ensure machine table",
			point: FaultRegistryEnsureMachineTable,
			run: func(t *testing.T, reg *Registry) error {
				return reg.EnsureMachineTable(t.Context())
			},
		},
		{
			name:  "ensure heartbeat table",
			point: FaultRegistryEnsureHeartbeatTable,
			run: func(t *testing.T, reg *Registry) error {
				return reg.EnsureHeartbeatTable(t.Context())
			},
		},
		{
			name:  "ensure network config table",
			point: FaultRegistryEnsureNetworkConfigTable,
			run: func(t *testing.T, reg *Registry) error {
				return reg.EnsureNetworkConfigTable(t.Context())
			},
		},
		{
			name:  "ensure network cidr",
			point: FaultRegistryEnsureNetworkCIDR,
			run: func(t *testing.T, reg *Registry) error {
				_, err := reg.EnsureNetworkCIDR(t.Context(), mustPrefix("10.42.0.0/16"), "", mustPrefix("10.0.0.0/16"))
				return err
			},
		},
		{
			name:  "upsert machine",
			point: FaultRegistryUpsertMachine,
			run: func(t *testing.T, reg *Registry) error {
				return reg.UpsertMachine(t.Context(), mesh.MachineRow{ID: "m1", PublicKey: "pk1"}, 0)
			},
		},
		{
			name:  "list machine rows",
			point: FaultRegistryListMachineRows,
			setup: func(t *testing.T, reg *Registry) {
				if err := reg.UpsertMachine(t.Context(), mesh.MachineRow{ID: "m1", PublicKey: "pk1"}, 0); err != nil {
					t.Fatalf("setup UpsertMachine() error = %v", err)
				}
			},
			run: func(t *testing.T, reg *Registry) error {
				rows, err := reg.ListMachineRows(t.Context())
				if err != nil {
					return err
				}
				if len(rows) == 0 {
					return errors.New("ListMachineRows() returned no rows")
				}
				return nil
			},
		},
		{
			name:  "subscribe machines",
			point: FaultRegistrySubscribeMachines,
			run: func(t *testing.T, reg *Registry) error {
				subCtx, cancel := context.WithCancel(t.Context())
				defer cancel()
				_, ch, err := reg.SubscribeMachines(subCtx)
				if err != nil {
					return err
				}
				if ch == nil {
					return errors.New("SubscribeMachines() returned nil channel")
				}
				return nil
			},
		},
		{
			name:  "subscribe heartbeats",
			point: FaultRegistrySubscribeHeartbeats,
			run: func(t *testing.T, reg *Registry) error {
				subCtx, cancel := context.WithCancel(t.Context())
				defer cancel()
				_, ch, err := reg.SubscribeHeartbeats(subCtx)
				if err != nil {
					return err
				}
				if ch == nil {
					return errors.New("SubscribeHeartbeats() returned nil channel")
				}
				return nil
			},
		},
		{
			name:  "bump heartbeat",
			point: FaultRegistryBumpHeartbeat,
			run: func(t *testing.T, reg *Registry) error {
				return reg.BumpHeartbeat(t.Context(), "node-a", "2025-01-01T00:00:00Z")
			},
		},
		{
			name:  "delete machine",
			point: FaultRegistryDeleteMachine,
			setup: func(t *testing.T, reg *Registry) {
				if err := reg.UpsertMachine(t.Context(), mesh.MachineRow{ID: "m1", PublicKey: "pk1"}, 0); err != nil {
					t.Fatalf("setup UpsertMachine() error = %v", err)
				}
			},
			run: func(t *testing.T, reg *Registry) error {
				return reg.DeleteMachine(t.Context(), "m1")
			},
		},
		{
			name:  "delete by endpoint except id",
			point: FaultRegistryDeleteByEndpointExceptID,
			setup: func(t *testing.T, reg *Registry) {
				if err := reg.UpsertMachine(t.Context(), mesh.MachineRow{ID: "m1", PublicKey: "pk1", Endpoint: "1.2.3.4:51820"}, 0); err != nil {
					t.Fatalf("setup UpsertMachine(m1) error = %v", err)
				}
				if err := reg.UpsertMachine(t.Context(), mesh.MachineRow{ID: "m2", PublicKey: "pk2", Endpoint: "1.2.3.4:51820"}, 0); err != nil {
					t.Fatalf("setup UpsertMachine(m2) error = %v", err)
				}
			},
			run: func(t *testing.T, reg *Registry) error {
				return reg.DeleteByEndpointExceptID(t.Context(), "1.2.3.4:51820", "m1")
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
			c := NewCluster(clock)
			reg := c.Registry("node-a")

			if tt.setup != nil {
				tt.setup(t, reg)
			}

			injected := errors.New("injected")
			reg.FailOnce(tt.point, injected)

			err := tt.run(t, reg)
			if !errors.Is(err, injected) {
				t.Fatalf("first call error = %v, want injected", err)
			}

			err = tt.run(t, reg)
			if err != nil {
				t.Fatalf("second call error = %v, want nil", err)
			}
		})
	}
}
