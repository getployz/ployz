package mesh

import (
	"context"
	"fmt"
	"net/netip"
)

// bridgeName returns the Linux bridge interface name for a Docker network ID.
func bridgeName(networkID string) string {
	return "br-" + networkID[:12]
}

// EnsureDockerNetwork creates or recreates a container overlay network.
// Returns the bridge interface name (e.g. "br-<id[:12]>").
func EnsureDockerNetwork(ctx context.Context, rt ContainerRuntime, name string, subnet netip.Prefix, wgIface string) (string, error) {
	nw, err := rt.NetworkInspect(ctx, name)
	if err != nil {
		return "", fmt.Errorf("inspect docker network %q: %w", name, err)
	}

	if nw.Exists && nw.Subnet == subnet.String() {
		return bridgeName(nw.ID), nil
	}

	if nw.Exists {
		if err := rt.NetworkRemove(ctx, name); err != nil {
			return "", err
		}
	}

	if err := rt.NetworkCreate(ctx, name, subnet, wgIface); err != nil {
		return "", err
	}
	nw, err = rt.NetworkInspect(ctx, name)
	if err != nil {
		return "", fmt.Errorf("inspect docker network %q after create: %w", name, err)
	}
	return bridgeName(nw.ID), nil
}

// CleanupDockerNetwork removes a container overlay network.
// Returns the bridge name if the network existed, empty string otherwise.
func CleanupDockerNetwork(ctx context.Context, rt ContainerRuntime, name string) (string, error) {
	nw, err := rt.NetworkInspect(ctx, name)
	if err != nil {
		return "", fmt.Errorf("inspect docker network %q: %w", name, err)
	}
	if !nw.Exists {
		return "", nil
	}
	bridge := bridgeName(nw.ID)
	if err := rt.NetworkRemove(ctx, name); err != nil {
		return "", err
	}
	return bridge, nil
}
