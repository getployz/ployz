// Package convergence watches membership changes and reconciles WireGuard peers.
//
// The loop consumes store subscriptions and reapplies a peer plan whenever the
// machine set changes.
package convergence
