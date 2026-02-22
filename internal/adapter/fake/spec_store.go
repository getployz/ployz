package fake

import (
	"sync"

	"ployz/internal/daemon/supervisor"
	"ployz/pkg/sdk/types"
)

var _ supervisor.SpecStore = (*SpecStore)(nil)

// SpecStore is an in-memory implementation of supervisor.SpecStore.
type SpecStore struct {
	CallRecorder
	mu    sync.Mutex
	specs map[string]supervisor.PersistedSpec

	SaveSpecErr   func(spec types.NetworkSpec, enabled bool) error
	GetSpecErr    func(network string) error
	ListSpecsErr  func() error
	DeleteSpecErr func(network string) error
}

// NewSpecStore creates a SpecStore with no stored specs.
func NewSpecStore() *SpecStore {
	return &SpecStore{specs: make(map[string]supervisor.PersistedSpec)}
}

func (s *SpecStore) SaveSpec(spec types.NetworkSpec, enabled bool) error {
	s.record("SaveSpec", spec, enabled)
	if s.SaveSpecErr != nil {
		if err := s.SaveSpecErr(spec, enabled); err != nil {
			return err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.specs[spec.Network] = supervisor.PersistedSpec{Spec: spec, Enabled: enabled}
	return nil
}

func (s *SpecStore) GetSpec(network string) (supervisor.PersistedSpec, bool, error) {
	s.record("GetSpec", network)
	if s.GetSpecErr != nil {
		if err := s.GetSpecErr(network); err != nil {
			return supervisor.PersistedSpec{}, false, err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	p, ok := s.specs[network]
	return p, ok, nil
}

func (s *SpecStore) ListSpecs() ([]supervisor.PersistedSpec, error) {
	s.record("ListSpecs")
	if s.ListSpecsErr != nil {
		if err := s.ListSpecsErr(); err != nil {
			return nil, err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]supervisor.PersistedSpec, 0, len(s.specs))
	for _, p := range s.specs {
		out = append(out, p)
	}
	return out, nil
}

func (s *SpecStore) DeleteSpec(network string) error {
	s.record("DeleteSpec", network)
	if s.DeleteSpecErr != nil {
		if err := s.DeleteSpecErr(network); err != nil {
			return err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.specs, network)
	return nil
}

func (s *SpecStore) Close() error {
	s.record("Close")
	return nil
}
