package network

// sqliteStateStore implements StateStore using the existing SQLite-backed
// loadState/saveState/deleteState functions.
type sqliteStateStore struct{}

func (sqliteStateStore) Load(dataDir string) (*State, error)        { return loadState(dataDir) }
func (sqliteStateStore) Save(dataDir string, s *State) error        { return saveState(dataDir, s) }
func (sqliteStateStore) Delete(dataDir string) error                { return deleteState(dataDir) }
