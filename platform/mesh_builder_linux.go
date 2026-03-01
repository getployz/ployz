//go:build linux

package platform

import (
	"context"
	"fmt"
	"net/netip"

	"ployz"
	"ployz/infra/corrorun"
	"ployz/infra/corrosion"
	"ployz/infra/overlay"
	"ployz/infra/store"
	"ployz/machine"
	"ployz/machine/convergence"
	"ployz/machine/mesh"
)

// NewMeshBuilder returns a builder that wires kernel WireGuard, a Corrosion
// child process, and convergence for Linux. Identity is captured in the closure.
func NewMeshBuilder(id machine.Identity, dataDir string) (machine.MeshBuilder, error) {
	return func(ctx context.Context) (machine.NetworkStack, error) {
		return buildMesh(id, dataDir)
	}, nil
}

func buildMesh(id machine.Identity, dataDir string) (*mesh.Mesh, error) {
	wg := NewWireGuard(id.PrivateKey)

	mgmtIP := ployz.ManagementIPFromKey(id.PrivateKey.PublicKey())
	gossipAddr := netip.AddrPortFrom(mgmtIP, corrorun.DefaultGossipPort)
	apiAddr := netip.AddrPortFrom(mgmtIP, corrorun.DefaultAPIPort)

	paths := corrorun.NewPaths(dataDir)
	if err := corrorun.WriteConfig(paths, store.Schema, gossipAddr, apiAddr, nil); err != nil {
		return nil, fmt.Errorf("write corrosion config: %w", err)
	}

	runtime := corrorun.NewExec(paths, apiAddr)

	corroClient, err := corrosion.NewClient(apiAddr)
	if err != nil {
		return nil, fmt.Errorf("create corrosion client: %w", err)
	}

	st := store.New(runtime, corroClient)

	pub := id.PrivateKey.PublicKey()
	self := ployz.MachineRecord{
		ID:        pub.String(),
		Name:      id.Name,
		PublicKey: pub,
		OverlayIP: mgmtIP,
	}
	conv := convergence.New(self,
		convergence.WithSubscriber(st),
		convergence.WithPeerSetter(wg),
		convergence.WithProber(wg),
	)

	return mesh.New(
		mesh.WithWireGuard(wg),
		mesh.WithStore(st),
		mesh.WithConvergence(conv),
		mesh.WithStoreHealth(st),
		mesh.WithOverlayNet(overlay.Host{}),
	), nil
}
