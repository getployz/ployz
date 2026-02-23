package fake

import (
	"encoding/json"
	"os"
	"sync"

	"ployz/internal/adapter/fake/fault"
	"ployz/internal/check"
	"ployz/internal/mesh"
)

var _ mesh.StateStore = (*StateStore)(nil)

const (
	FaultStateStoreLoad   = "state_store.load"
	FaultStateStoreSave   = "state_store.save"
	FaultStateStoreDelete = "state_store.delete"
)

// StateStore is an in-memory implementation of mesh.StateStore.
type StateStore struct {
	CallRecorder
	mu     sync.Mutex
	states map[string]*mesh.State
	faults *fault.Injector

	LoadErr   func(dataDir string) error
	SaveErr   func(dataDir string, s *mesh.State) error
	DeleteErr func(dataDir string) error
}

// NewStateStore creates a StateStore with no stored state.
func NewStateStore() *StateStore {
	return &StateStore{states: make(map[string]*mesh.State), faults: fault.NewInjector()}
}

func (s *StateStore) FailOnce(point string, err error) {
	s.faults.FailOnce(point, err)
}

func (s *StateStore) FailAlways(point string, err error) {
	s.faults.FailAlways(point, err)
}

func (s *StateStore) SetFaultHook(point string, hook fault.Hook) {
	s.faults.SetHook(point, hook)
}

func (s *StateStore) ClearFault(point string) {
	s.faults.Clear(point)
}

func (s *StateStore) ResetFaults() {
	s.faults.Reset()
}

func (s *StateStore) evalFault(point string, args ...any) error {
	check.Assert(s != nil, "StateStore.evalFault: receiver must not be nil")
	check.Assert(s.faults != nil, "StateStore.evalFault: faults injector must not be nil")
	if s == nil || s.faults == nil {
		return nil
	}
	return s.faults.Eval(point, args...)
}

func (s *StateStore) Load(dataDir string) (*mesh.State, error) {
	s.record("Load", dataDir)
	if err := s.evalFault(FaultStateStoreLoad, dataDir); err != nil {
		return nil, err
	}
	if s.LoadErr != nil {
		if err := s.LoadErr(dataDir); err != nil {
			return nil, err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	st, ok := s.states[dataDir]
	if !ok {
		return nil, os.ErrNotExist
	}
	return deepCopyState(st), nil
}

func (s *StateStore) Save(dataDir string, st *mesh.State) error {
	s.record("Save", dataDir, st)
	if err := s.evalFault(FaultStateStoreSave, dataDir, st); err != nil {
		return err
	}
	if s.SaveErr != nil {
		if err := s.SaveErr(dataDir, st); err != nil {
			return err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	s.states[dataDir] = deepCopyState(st)
	return nil
}

func (s *StateStore) Delete(dataDir string) error {
	s.record("Delete", dataDir)
	if err := s.evalFault(FaultStateStoreDelete, dataDir); err != nil {
		return err
	}
	if s.DeleteErr != nil {
		if err := s.DeleteErr(dataDir); err != nil {
			return err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	delete(s.states, dataDir)
	return nil
}

func (s *StateStore) StatePath(dataDir string) string {
	s.record("StatePath", dataDir)
	return "fake://" + dataDir
}

func deepCopyState(s *mesh.State) *mesh.State {
	data, _ := json.Marshal(s)
	var out mesh.State
	_ = json.Unmarshal(data, &out)
	return &out
}
