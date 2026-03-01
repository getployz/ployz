// Package wireguard contains shared helpers used by OS-specific WireGuard adapters.
//
// Concrete mesh.WireGuard implementations live in subpackages:
//   - kernel (linux) — kernel WireGuard via netlink/wgctrl
//   - container (darwin) — WireGuard in a Docker container
//   - stub (!linux && !darwin)
package wireguard
