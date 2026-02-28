// Package store implements machine.ClusterStore backed by Corrosion.
package store

import (
	_ "embed"

	"ployz/platform/corrosion"
)

// Schema is the SQL schema for the machines table. Embed this into
// Corrosion's schema_paths directory so it gets applied on startup.
//
//go:embed schema.sql
var Schema string

// Store is a cluster store backed by Corrosion.
type Store struct {
	client *corrosion.Client
}

// New creates a Store using the given Corrosion client.
func New(client *corrosion.Client) *Store {
	return &Store{client: client}
}
