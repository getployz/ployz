package fake

import (
	"sync"

	"ployz/internal/adapter/fake/fault"
	"ployz/internal/check"
	"ployz/internal/daemon/supervisor"
	"ployz/pkg/sdk/types"
)

var _ supervisor.SpecStore = (*SpecStore)(nil)

const (
	FaultSpecStoreSaveSpec   = "spec_store.save_spec"
	FaultSpecStoreGetSpec    = "spec_store.get_spec"
	FaultSpecStoreListSpecs  = "spec_store.list_specs"
	FaultSpecStoreDeleteSpec = "spec_store.delete_spec"
)

// SpecStore is an in-memory implementation of supervisor.SpecStore.
type SpecStore struct {
	CallRecorder
	mu     sync.Mutex
	specs  map[string]supervisor.PersistedSpec
	faults *fault.Injector

	SaveSpecErr   func(spec types.NetworkSpec, enabled bool) error
	GetSpecErr    func(network string) error
	ListSpecsErr  func() error
	DeleteSpecErr func(network string) error
}

// NewSpecStore creates a SpecStore with no stored specs.
func NewSpecStore() *SpecStore {
	return &SpecStore{specs: make(map[string]supervisor.PersistedSpec), faults: fault.NewInjector()}
}

func (s *SpecStore) FailOnce(point string, err error) {
	s.faults.FailOnce(point, err)
}

func (s *SpecStore) FailAlways(point string, err error) {
	s.faults.FailAlways(point, err)
}

func (s *SpecStore) SetFaultHook(point string, hook fault.Hook) {
	s.faults.SetHook(point, hook)
}

func (s *SpecStore) ClearFault(point string) {
	s.faults.Clear(point)
}

func (s *SpecStore) ResetFaults() {
	s.faults.Reset()
}

func (s *SpecStore) evalFault(point string, args ...any) error {
	check.Assert(s != nil, "SpecStore.evalFault: receiver must not be nil")
	check.Assert(s.faults != nil, "SpecStore.evalFault: faults injector must not be nil")
	if s == nil || s.faults == nil {
		return nil
	}
	return s.faults.Eval(point, args...)
}

func (s *SpecStore) SaveSpec(spec types.NetworkSpec, enabled bool) error {
	s.record("SaveSpec", spec, enabled)
	if err := s.evalFault(FaultSpecStoreSaveSpec, spec, enabled); err != nil {
		return err
	}
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
	if err := s.evalFault(FaultSpecStoreGetSpec, network); err != nil {
		return supervisor.PersistedSpec{}, false, err
	}
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
	if err := s.evalFault(FaultSpecStoreListSpecs); err != nil {
		return nil, err
	}
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
	if err := s.evalFault(FaultSpecStoreDeleteSpec, network); err != nil {
		return err
	}
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
