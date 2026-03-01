// Package machine owns local machine identity, persisted state, and lifecycle.
//
// A Machine may run standalone or attach a mesh stack. mesh lifecycle orchestration
// lives in machine/mesh; peer reconciliation lives in machine/convergence.
package machine
