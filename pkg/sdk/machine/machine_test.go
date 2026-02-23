package machine_test

import (
	"context"
	"fmt"
	"net/netip"
	"testing"
	"testing/synctest"

	"ployz/internal/mesh"
	"ployz/internal/testkit/scenario"
	"ployz/pkg/sdk/client"
	"ployz/pkg/sdk/machine"
	"ployz/pkg/sdk/types"
)

func mustNode(t *testing.T, s *scenario.Scenario, id string) *scenario.Node {
	t.Helper()
	node := s.Node(id)
	if node == nil {
		t.Fatalf("missing scenario node %q", id)
	}
	return node
}

func TestAddMachine_Success(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		ctx := t.Context()
		t.Cleanup(synctest.Wait)

		s := scenario.MustNew(t, ctx, scenario.Config{
			NodeIDs:      []string{"local", "remote"},
			DataRootBase: "/tmp/ployz-machine-success",
		})

		local := mustNode(t, s, "local")
		remote := mustNode(t, s, "remote")

		_, err := local.Manager.ApplyNetworkSpec(ctx, types.NetworkSpec{
			Network:     "default",
			DataRoot:    local.DataRoot,
			NetworkCIDR: "10.210.0.0/16",
			Subnet:      "10.210.1.0/24",
			WGPort:      51820,
		})
		if err != nil {
			t.Fatalf("local apply: %v", err)
		}
		synctest.Wait()
		s.Drain()
		synctest.Wait()

		localID, err := local.Manager.GetIdentity(ctx, "default")
		if err != nil {
			t.Fatalf("local identity: %v", err)
		}

		svc := machine.New(local.Manager)
		result, err := svc.AddMachine(ctx, machine.AddOptions{
			Network:  "default",
			DataRoot: remote.DataRoot,
			Endpoint: fmt.Sprintf("1.2.3.4:%d", localID.WGPort),
			ConnectFunc: func(ctx context.Context) (client.API, error) {
				return remote.Manager, nil
			},
		})
		if err != nil {
			t.Fatalf("AddMachine: %v", err)
		}
		if result.Machine.ID == "" {
			t.Error("expected non-empty machine ID")
		}
		if result.Peers < 2 {
			t.Errorf("expected at least 2 peers, got %d", result.Peers)
		}
	})
}

func TestAddMachine_FailsWithoutRemoteSeed(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		ctx := t.Context()
		t.Cleanup(synctest.Wait)

		s := scenario.MustNew(t, ctx, scenario.Config{
			NodeIDs:      []string{"local", "remote"},
			DataRootBase: "/tmp/ployz-machine-seed-failure",
		})

		local := mustNode(t, s, "local")
		remote := mustNode(t, s, "remote")

		_, err := local.Manager.ApplyNetworkSpec(ctx, types.NetworkSpec{
			Network:     "default",
			DataRoot:    local.DataRoot,
			NetworkCIDR: "10.210.0.0/16",
			Subnet:      "10.210.1.0/24",
			WGPort:      51820,
		})
		if err != nil {
			t.Fatalf("local apply: %v", err)
		}
		synctest.Wait()
		s.Drain()
		synctest.Wait()

		localID, err := local.Manager.GetIdentity(ctx, "default")
		if err != nil {
			t.Fatalf("local identity: %v", err)
		}

		remoteReg := s.Cluster.Registry("remote")
		remoteReg.UpsertMachineErr = func(ctx context.Context, row mesh.MachineRow, ver int64) error {
			if row.PublicKey == localID.PublicKey {
				return fmt.Errorf("injected: refusing local seed")
			}
			return nil
		}

		svc := machine.New(local.Manager)
		_, err = svc.AddMachine(ctx, machine.AddOptions{
			Network:  "default",
			DataRoot: remote.DataRoot,
			Endpoint: fmt.Sprintf("1.2.3.4:%d", localID.WGPort),
			ConnectFunc: func(ctx context.Context) (client.API, error) {
				return remote.Manager, nil
			},
		})
		if err == nil {
			t.Fatal("expected AddMachine to fail when remote seed is rejected")
		}
		t.Logf("expected error: %v", err)
	})
}

func TestGossipBlockedUntilReconcile(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		ctx := t.Context()
		t.Cleanup(synctest.Wait)

		s := scenario.MustNew(t, ctx, scenario.Config{
			NodeIDs:      []string{"A", "B"},
			DataRootBase: "/tmp/ployz-machine-gossip",
		})

		nodeA := mustNode(t, s, "A")
		nodeB := mustNode(t, s, "B")

		publicKeys := make(map[string]string, 2)
		s.Cluster.SetLinkPredicate(func(from, to string) bool {
			fromPK, ok := publicKeys[from]
			if !ok {
				return false
			}
			switch to {
			case "A":
				return nodeA.PlatformOps.HasPeer(fromPK)
			case "B":
				return nodeB.PlatformOps.HasPeer(fromPK)
			}
			return false
		})

		_, err := nodeA.Manager.ApplyNetworkSpec(ctx, types.NetworkSpec{
			Network:     "default",
			DataRoot:    nodeA.DataRoot,
			NetworkCIDR: "10.210.0.0/16",
			Subnet:      "10.210.1.0/24",
			WGPort:      51820,
		})
		if err != nil {
			t.Fatalf("A apply: %v", err)
		}
		synctest.Wait()

		idA, err := nodeA.Manager.GetIdentity(ctx, "default")
		if err != nil {
			t.Fatalf("A identity: %v", err)
		}
		publicKeys["A"] = idA.PublicKey

		mgmtA, _ := netip.ParseAddr(idA.ManagementIP)
		bootstrapA := netip.AddrPortFrom(mgmtA, 4001).String()

		_, err = nodeB.Manager.ApplyNetworkSpec(ctx, types.NetworkSpec{
			Network:     "default",
			DataRoot:    nodeB.DataRoot,
			NetworkCIDR: "10.210.0.0/16",
			Subnet:      "10.210.2.0/24",
			WGPort:      51820,
			Bootstrap:   []string{bootstrapA},
		})
		if err != nil {
			t.Fatalf("B apply: %v", err)
		}
		synctest.Wait()

		idB, err := nodeB.Manager.GetIdentity(ctx, "default")
		if err != nil {
			t.Fatalf("B identity: %v", err)
		}
		publicKeys["B"] = idB.PublicKey

		err = nodeA.Manager.UpsertMachine(ctx, "default", types.MachineEntry{
			ID:        idB.PublicKey,
			PublicKey: idB.PublicKey,
			Subnet:    idB.Subnet,
			Endpoint:  "2.2.2.2:51820",
		})
		if err != nil {
			t.Fatalf("A upsert B: %v", err)
		}

		s.Drain()
		synctest.Wait()

		snapB := s.Cluster.Snapshot("B")
		if _, found := snapB.Machine(idA.PublicKey); found {
			t.Error("B should NOT see A's machine before reconciling A as WG peer")
		}

		err = nodeB.Manager.UpsertMachine(ctx, "default", types.MachineEntry{
			ID:        idA.PublicKey,
			PublicKey: idA.PublicKey,
			Subnet:    idA.Subnet,
			Endpoint:  "1.1.1.1:51820",
		})
		if err != nil {
			t.Fatalf("B upsert A (seed): %v", err)
		}

		err = nodeB.Manager.TriggerReconcile(ctx, "default")
		if err != nil {
			t.Fatalf("B TriggerReconcile: %v", err)
		}
		synctest.Wait()

		if !nodeB.PlatformOps.HasPeer(idA.PublicKey) {
			t.Fatal("B should have A as WG peer after TriggerReconcile")
		}

		s.Drain()
		synctest.Wait()

		snapB = s.Cluster.Snapshot("B")
		if _, found := snapB.Machine(idA.PublicKey); !found {
			t.Error("B should see A's machine after reconciling A as WG peer and draining")
		}
	})
}
