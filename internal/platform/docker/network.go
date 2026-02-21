package docker

import (
	"context"
	"fmt"
	"net/netip"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types/container"
	dockernetwork "github.com/docker/docker/api/types/network"
	"github.com/docker/docker/client"
)

func PurgeNetworkContainers(ctx context.Context, cli *client.Client, networkName string, nw dockernetwork.Inspect) error {
	for id := range nw.Containers {
		if err := cli.ContainerRemove(ctx, id, container.RemoveOptions{Force: true, RemoveVolumes: true}); err != nil {
			if errdefs.IsNotFound(err) {
				continue
			}
			return fmt.Errorf("remove container %q attached to docker network %q: %w", id, networkName, err)
		}
	}
	return nil
}

func EnsureNetwork(ctx context.Context, cli *client.Client, name string, subnet netip.Prefix, wgIface string) (string, error) {
	needsCreate := false
	nw, err := cli.NetworkInspect(ctx, name, dockernetwork.InspectOptions{})
	if err != nil {
		if !errdefs.IsNotFound(err) {
			return "", fmt.Errorf("inspect docker network %q: %w", name, err)
		}
		needsCreate = true
	} else if len(nw.IPAM.Config) == 0 || nw.IPAM.Config[0].Subnet != subnet.String() {
		if err := PurgeNetworkContainers(ctx, cli, name, nw); err != nil {
			return "", err
		}
		if err := cli.NetworkRemove(ctx, name); err != nil {
			return "", fmt.Errorf("remove old docker network %q: %w", name, err)
		}
		needsCreate = true
	}

	if needsCreate {
		if _, err := cli.NetworkCreate(ctx, name, dockernetwork.CreateOptions{
			Driver: "bridge",
			Scope:  "local",
			IPAM:   &dockernetwork.IPAM{Config: []dockernetwork.IPAMConfig{{Subnet: subnet.String()}}},
			Options: map[string]string{
				"com.docker.network.bridge.trusted_host_interfaces": wgIface,
			},
		}); err != nil {
			return "", fmt.Errorf("create docker network %q: %w", name, err)
		}
		nw, err = cli.NetworkInspect(ctx, name, dockernetwork.InspectOptions{})
		if err != nil {
			return "", fmt.Errorf("inspect docker network %q: %w", name, err)
		}
	}

	return "br-" + nw.ID[:12], nil
}

func CleanupNetwork(ctx context.Context, cli *client.Client, name string) (string, error) {
	nw, err := cli.NetworkInspect(ctx, name, dockernetwork.InspectOptions{})
	if err != nil {
		if errdefs.IsNotFound(err) {
			return "", nil
		}
		return "", fmt.Errorf("inspect docker network %q: %w", name, err)
	}
	if err := PurgeNetworkContainers(ctx, cli, name, nw); err != nil {
		return "", err
	}
	bridge := "br-" + nw.ID[:12]
	if err := cli.NetworkRemove(ctx, name); err != nil && !errdefs.IsNotFound(err) {
		return "", fmt.Errorf("remove docker network %q: %w", name, err)
	}
	return bridge, nil
}
