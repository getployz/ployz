// Package store implements mesh.Store backed by Corrosion.
//
// The composed Store wraps a Runtime (process lifecycle from corrorun)
// and a Corrosion client (data access). Together they satisfy mesh.Store.
package store

import (
	"context"
	_ "embed"

	"ployz/infra/corrosion"
)

// Schema is the SQL schema for the machines table. Embed this into
// Corrosion's schema_paths directory so it gets applied on startup.
//
//go:embed schema.sql
var Schema string

// Runtime manages the Corrosion process lifecycle.
// corrorun.Container, corrorun.Exec, and corrorun.Service implement this.
type Runtime interface {
	Start(ctx context.Context) error
	Stop(ctx context.Context) error
}

// Store is a mesh.Store backed by Corrosion. It composes a Runtime
// (process lifecycle) with a Corrosion client (data access).
type Store struct {
	runtime Runtime
	client  *corrosion.Client
}

// New creates a Store that manages the given runtime and queries via client.
func New(runtime Runtime, client *corrosion.Client) *Store {
	return &Store{runtime: runtime, client: client}
}

// Start launches the Corrosion process via the runtime.
func (s *Store) Start(ctx context.Context) error {
	return s.runtime.Start(ctx)
}

// Stop tears down the Corrosion process via the runtime.
func (s *Store) Stop(ctx context.Context) error {
	return s.runtime.Stop(ctx)
}
