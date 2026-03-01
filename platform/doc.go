// Package platform defines compile-time OS defaults and platform selection.
//
// Platform split:
//   - linux: kernel WireGuard backend (infra/wireguard/kernel)
//   - darwin: userspace WireGuard backend (infra/wireguard/user)
//   - other: stub WireGuard backend (infra/wireguard/stub)
//
// platform chooses concrete implementations and constants. Runtime side effects
// remain in machine and infra packages.
package platform
