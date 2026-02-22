package fake

import (
	"encoding/json"
	"os"
	"sync"

	"ployz/internal/mesh"
)

var _ mesh.StateStore = (*StateStore)(nil)

// StateStore is an in-memory implementation of mesh.StateStore.
type StateStore struct {
	CallRecorder
	mu     sync.Mutex
	states map[string]*mesh.State

	LoadErr   func(dataDir string) error
	SaveErr   func(dataDir string, s *mesh.State) error
	DeleteErr func(dataDir string) error
}

// NewStateStore creates a StateStore with no stored state.
func NewStateStore() *StateStore {
	return &StateStore{states: make(map[string]*mesh.State)}
}

func (s *StateStore) Load(dataDir string) (*mesh.State, error) {
	s.record("Load", dataDir)
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
