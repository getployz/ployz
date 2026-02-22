package machine_test

import (
	"context"
	"fmt"
	"net/netip"
	"testing"
	"testing/synctest"

	"ployz/internal/adapter/fake"
	"ployz/internal/daemon/supervisor"
	"ployz/internal/engine"
	"ployz/internal/mesh"
	"ployz/internal/reconcile"
	"ployz/pkg/sdk/client"
	"ployz/pkg/sdk/machine"
	"ployz/pkg/sdk/types"
)

// testNode holds all fakes and real components for a single simulated node.
type testNode struct {
	manager    *supervisor.Manager
	platformOps *fake.PlatformOps
}

// setupNode creates a real Manager backed by fake leaf adapters.
// The cluster parameter must already have the nodeID registered.
func setupNode(ctx context.Context, t *testing.T, nodeID string, cluster *fake.Cluster, dataRoot string) testNode {
	t.Helper()

	stateStore := fake.NewStateStore()
	specStore := fake.NewSpecStore()
	platformOps := &fake.PlatformOps{}
	containerRT := fake.NewContainerRuntime()
	corrosionRT := fake.NewCorrosionRuntime()
	statusProber := &fake.StatusProber{WG: true, DockerNet: true, Corrosion: true}

	newCtrl := func(opts ...mesh.Option) (*mesh.Controller, error) {
		allOpts := []mesh.Option{
			mesh.WithStateStore(stateStore),
			mesh.WithPlatformOps(platformOps),
			mesh.WithContainerRuntime(containerRT),
			mesh.WithCorrosionRuntime(corrosionRT),
			mesh.WithStatusProber(statusProber),
			mesh.WithRegistryFactory(cluster.NetworkRegistryFactory(nodeID)),
			mesh.WithClock(mesh.RealClock{}),
		}
		allOpts = append(allOpts, opts...)
		return mesh.New(allOpts...)
	}

	ctrl, err := newCtrl()
	if err != nil {
		t.Fatalf("create controller for %s: %v", nodeID, err)
	}

	eng := engine.New(ctx,
		engine.WithControllerFactory(func() (engine.NetworkController, error) {
			return newCtrl()
		}),
		engine.WithPeerReconcilerFactory(func() (reconcile.PeerReconciler, error) {
			return newCtrl()
		}),
		engine.WithRegistryFactory(cluster.ReconcileRegistryFactory(nodeID)),
		engine.WithStateStore(stateStore),
		engine.WithClock(mesh.RealClock{}),
		engine.WithPingDialFunc(cluster.DialFunc(nodeID)),
		engine.WithNTPCheckFunc(func() reconcile.NTPStatus {
			return reconcile.NTPStatus{Healthy: true}
		}),
	)

	mgr, err := supervisor.New(ctx, dataRoot,
		supervisor.WithSpecStore(specStore),
		supervisor.WithManagerStateStore(stateStore),
		supervisor.WithManagerController(ctrl),
		supervisor.WithManagerEngine(eng),
	)
	if err != nil {
		t.Fatalf("create manager for %s: %v", nodeID, err)
	}

	return testNode{manager: mgr, platformOps: platformOps}
}

func TestAddMachine_Success(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		defer synctest.Wait()
		defer cancel()
		cluster := fake.NewCluster(mesh.RealClock{})

		// Create local and remote nodes.
		local := setupNode(ctx, t, "local", cluster, "/tmp/test-local")
		remote := setupNode(ctx, t, "remote", cluster, "/tmp/test-remote")

		// Initialize local network.
		_, err := local.manager.ApplyNetworkSpec(ctx, types.NetworkSpec{
			Network:     "default",
			DataRoot:    "/tmp/test-local",
			NetworkCIDR: "10.210.0.0/16",
			Subnet:      "10.210.1.0/24",
			WGPort:      51820,
		})
		if err != nil {
			t.Fatalf("local apply: %v", err)
		}
		synctest.Wait()
		cluster.Drain()
		synctest.Wait()

		// Get local identity for the remote endpoint.
		localID, err := local.manager.GetIdentity(ctx, "default")
		if err != nil {
			t.Fatalf("local identity: %v", err)
		}

		svc := machine.New(local.manager)
		result, err := svc.AddMachine(ctx, machine.AddOptions{
			Network:  "default",
			DataRoot: "/tmp/test-remote",
			Endpoint: fmt.Sprintf("1.2.3.4:%d", localID.WGPort),
			ConnectFunc: func(ctx context.Context) (client.API, error) {
				return remote.manager, nil
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
		ctx, cancel := context.WithCancel(context.Background())
		defer synctest.Wait()
		defer cancel()
		cluster := fake.NewCluster(mesh.RealClock{})

		local := setupNode(ctx, t, "local", cluster, "/tmp/test-local")
		remote := setupNode(ctx, t, "remote", cluster, "/tmp/test-remote")

		_, err := local.manager.ApplyNetworkSpec(ctx, types.NetworkSpec{
			Network:     "default",
			DataRoot:    "/tmp/test-local",
			NetworkCIDR: "10.210.0.0/16",
			Subnet:      "10.210.1.0/24",
			WGPort:      51820,
		})
		if err != nil {
			t.Fatalf("local apply: %v", err)
		}
		synctest.Wait()
		cluster.Drain()
		synctest.Wait()

		localID, err := local.manager.GetIdentity(ctx, "default")
		if err != nil {
			t.Fatalf("local identity: %v", err)
		}

		// Get the remote registry and inject an error on UpsertMachine
		// so the "seed local machine on remote" step fails.
		remoteReg := cluster.Registry("remote")
		remoteReg.UpsertMachineErr = func(ctx context.Context, row mesh.MachineRow, ver int64) error {
			// Only fail upserts that look like the local machine seed.
			if row.PublicKey == localID.PublicKey {
				return fmt.Errorf("injected: refusing local seed")
			}
			return nil
		}

		svc := machine.New(local.manager)
		_, err = svc.AddMachine(ctx, machine.AddOptions{
			Network:  "default",
			DataRoot: "/tmp/test-remote",
			Endpoint: fmt.Sprintf("1.2.3.4:%d", localID.WGPort),
			ConnectFunc: func(ctx context.Context) (client.API, error) {
				return remote.manager, nil
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
		ctx, cancel := context.WithCancel(context.Background())
		defer synctest.Wait()
		defer cancel()
		cluster := fake.NewCluster(mesh.RealClock{})

		nodeA := setupNode(ctx, t, "A", cluster, "/tmp/test-a")
		nodeB := setupNode(ctx, t, "B", cluster, "/tmp/test-b")

		// Enable link predicate: gossip only flows from X→Y if Y has
		// reconciled X as a WG peer (i.e., Y's PlatformOps.HasPeer(X.PublicKey)).
		// This models the real constraint: Corrosion gossip requires WG connectivity.
		publicKeys := make(map[string]string) // nodeID → publicKey

		cluster.SetLinkPredicate(func(from, to string) bool {
			fromPK, ok := publicKeys[from]
			if !ok {
				return false // unknown source
			}
			switch to {
			case "A":
				return nodeA.platformOps.HasPeer(fromPK)
			case "B":
				return nodeB.platformOps.HasPeer(fromPK)
			}
			return false
		})

		// Initialize node A.
		_, err := nodeA.manager.ApplyNetworkSpec(ctx, types.NetworkSpec{
			Network:     "default",
			DataRoot:    "/tmp/test-a",
			NetworkCIDR: "10.210.0.0/16",
			Subnet:      "10.210.1.0/24",
			WGPort:      51820,
		})
		if err != nil {
			t.Fatalf("A apply: %v", err)
		}
		synctest.Wait()

		idA, err := nodeA.manager.GetIdentity(ctx, "default")
		if err != nil {
			t.Fatalf("A identity: %v", err)
		}
		publicKeys["A"] = idA.PublicKey

		// Derive bootstrap from A's management IP.
		mgmtA, _ := netip.ParseAddr(idA.ManagementIP)
		bootstrapA := netip.AddrPortFrom(mgmtA, 4001).String()

		// Initialize node B with A as bootstrap.
		_, err = nodeB.manager.ApplyNetworkSpec(ctx, types.NetworkSpec{
			Network:     "default",
			DataRoot:    "/tmp/test-b",
			NetworkCIDR: "10.210.0.0/16",
			Subnet:      "10.210.2.0/24",
			WGPort:      51820,
			Bootstrap:   []string{bootstrapA},
		})
		if err != nil {
			t.Fatalf("B apply: %v", err)
		}
		synctest.Wait()

		idB, err := nodeB.manager.GetIdentity(ctx, "default")
		if err != nil {
			t.Fatalf("B identity: %v", err)
		}
		publicKeys["B"] = idB.PublicKey

		// A upserts B's machine entry.
		err = nodeA.manager.UpsertMachine(ctx, "default", types.MachineEntry{
			ID:        idB.PublicKey,
			PublicKey: idB.PublicKey,
			Subnet:    idB.Subnet,
			Endpoint:  "2.2.2.2:51820",
		})
		if err != nil {
			t.Fatalf("A upsert B: %v", err)
		}

		// Drain — but link predicate blocks A→B because B hasn't reconciled A as peer.
		cluster.Drain()
		synctest.Wait()

		snapB := cluster.Snapshot("B")
		if _, found := snapB.Machine(idB.PublicKey); found {
			// B should only see its own machine from its own apply,
			// but should NOT see A's machine yet.
		}
		// Verify B does NOT have A's machine row.
		if _, found := snapB.Machine(idA.PublicKey); found {
			t.Error("B should NOT see A's machine before reconciling A as WG peer")
		}

		// Now B runs TriggerReconcile which calls ctrl.Reconcile → ApplyPeerConfig.
		// This makes B's PlatformOps record A as a WG peer.
		// But first we need B to know about A. Seed A into B directly (simulating converge step).
		err = nodeB.manager.UpsertMachine(ctx, "default", types.MachineEntry{
			ID:        idA.PublicKey,
			PublicKey: idA.PublicKey,
			Subnet:    idA.Subnet,
			Endpoint:  "1.1.1.1:51820",
		})
		if err != nil {
			t.Fatalf("B upsert A (seed): %v", err)
		}

		// Now trigger reconcile on B — this calls Reconcile → ApplyPeerConfig which
		// records A's public key in PlatformOps.Peers.
		err = nodeB.manager.TriggerReconcile(ctx, "default")
		if err != nil {
			t.Fatalf("B TriggerReconcile: %v", err)
		}
		synctest.Wait()

		// Verify B now has A as a WG peer.
		if !nodeB.platformOps.HasPeer(idA.PublicKey) {
			t.Fatal("B should have A as WG peer after TriggerReconcile")
		}

		// Now drain again — link predicate should pass for A→B.
		cluster.Drain()
		synctest.Wait()

		snapB = cluster.Snapshot("B")
		if _, found := snapB.Machine(idA.PublicKey); !found {
			t.Error("B should see A's machine after reconciling A as WG peer and draining")
		}
	})
}
