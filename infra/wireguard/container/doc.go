// Package container implements mesh.WireGuard by running WireGuard in a
// Docker container. Used on macOS where the Docker VM (OrbStack) provides
// kernel WireGuard support. The container manages a WireGuard interface
// inside the VM and is configured via docker exec.
package container
