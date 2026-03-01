// Package wireguard contains shared helpers used by OS-specific WireGuard adapters.
//
// Concrete mesh.WireGuard implementations live in subpackages:
//   - kernel (linux)
//   - user (darwin)
//   - stub (!linux && !darwin)
//
// This package also contains privileged-helper plumbing used by platform wiring.
package wireguard
