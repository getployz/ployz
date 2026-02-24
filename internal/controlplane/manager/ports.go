package manager

import "ployz/pkg/sdk/types"

// SpecStore persists network specs with enabled/disabled state.
type SpecStore interface {
	SaveSpec(spec types.NetworkSpec, enabled bool) error
	GetSpec() (PersistedSpec, bool, error)
	DeleteSpec() error
	Close() error
}

// PersistedSpec holds a network spec and its enabled state.
type PersistedSpec struct {
	Spec    types.NetworkSpec
	Enabled bool
}
