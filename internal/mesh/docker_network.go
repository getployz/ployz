package mesh

import (
	"context"
	"fmt"
	"net/netip"
)

// EnsureDockerNetwork creates or recreates a container overlay network.
// Returns the bridge interface name (e.g. "br-<id[:12]>").
func EnsureDockerNetwork(ctx context.Context, rt ContainerRuntime, name string, subnet netip.Prefix, wgIface string) (string, error) {
	nw, err := rt.NetworkInspect(ctx, name)
	if err != nil {
		return "", fmt.Errorf("inspect docker network %q: %w", name, err)
	}

	if nw.Exists && nw.Subnet == subnet.String() {
		return "br-" + nw.ID[:12], nil
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
	return "br-" + nw.ID[:12], nil
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
	bridge := "br-" + nw.ID[:12]
	if rmErr := rt.NetworkRemove(ctx, name); rmErr != nil {
		return "", rmErr
	}
	return bridge, nil
}
