package mesh

import (
	"context"
	"errors"
	"testing"

	"ployz"
)

// --- fakes ---

type call struct{ method string }

type fakeWG struct {
	calls   []call
	upErr   error
	downErr error
}

func (f *fakeWG) Up(context.Context) error {
	f.calls = append(f.calls, call{"Up"})
	return f.upErr
}

func (f *fakeWG) SetPeers(context.Context, []ployz.MachineRecord) error {
	f.calls = append(f.calls, call{"SetPeers"})
	return nil
}

func (f *fakeWG) Down(context.Context) error {
	f.calls = append(f.calls, call{"Down"})
	return f.downErr
}

type fakeStore struct {
	calls    []call
	startErr error
	stopErr  error
}

func (f *fakeStore) Start(context.Context) error {
	f.calls = append(f.calls, call{"Start"})
	return f.startErr
}

func (f *fakeStore) Stop(context.Context) error {
	f.calls = append(f.calls, call{"Stop"})
	return f.stopErr
}

func (f *fakeStore) ListMachines(context.Context) ([]ployz.MachineRecord, error) {
	return nil, nil
}

func (f *fakeStore) SubscribeMachines(context.Context) ([]ployz.MachineRecord, <-chan ployz.MachineEvent, error) {
	return nil, nil, nil
}

func (f *fakeStore) UpsertMachine(context.Context, ployz.MachineRecord) error { return nil }

func (f *fakeStore) DeleteMachine(context.Context, string) error { return nil }

type fakeConvergence struct {
	calls   []call
	stopErr error
}

func (f *fakeConvergence) Start(context.Context) error {
	f.calls = append(f.calls, call{"Start"})
	return nil
}

func (f *fakeConvergence) Stop() error {
	f.calls = append(f.calls, call{"Stop"})
	return f.stopErr
}

// --- helpers ---

func methods(calls []call) []string {
	out := make([]string, len(calls))
	for i, c := range calls {
		out[i] = c.method
	}
	return out
}

func sliceEqual(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

// --- tests ---

func TestStop_OnlyStopsConvergence(t *testing.T) {
	wg := &fakeWG{}
	store := &fakeStore{}
	conv := &fakeConvergence{}

	m := New(WithWireGuard(wg), WithStore(store), WithConvergence(conv))

	if err := m.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	if err := m.Stop(context.Background()); err != nil {
		t.Fatalf("Stop: %v", err)
	}

	// Convergence must be stopped.
	if got := methods(conv.calls); !sliceEqual(got, []string{"Start", "Stop"}) {
		t.Errorf("convergence calls = %v, want [Start Stop]", got)
	}

	// Store and WireGuard must NOT be touched during Stop.
	if got := methods(store.calls); !sliceEqual(got, []string{"Start"}) {
		t.Errorf("store calls = %v, want [Start] (no Stop)", got)
	}
	if got := methods(wg.calls); !sliceEqual(got, []string{"Up"}) {
		t.Errorf("wg calls = %v, want [Up] (no Down)", got)
	}

	if m.Phase() != PhaseStopped {
		t.Errorf("phase = %s, want stopped", m.Phase())
	}
}

func TestDestroy_TearsDownInReverseOrder(t *testing.T) {
	wg := &fakeWG{}
	store := &fakeStore{}
	conv := &fakeConvergence{}

	m := New(WithWireGuard(wg), WithStore(store), WithConvergence(conv))

	if err := m.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	if err := m.Destroy(context.Background()); err != nil {
		t.Fatalf("Destroy: %v", err)
	}

	// Verify reverse order: convergence stop, store stop, WG down.
	if got := methods(conv.calls); !sliceEqual(got, []string{"Start", "Stop"}) {
		t.Errorf("convergence calls = %v, want [Start Stop]", got)
	}
	if got := methods(store.calls); !sliceEqual(got, []string{"Start", "Stop"}) {
		t.Errorf("store calls = %v, want [Start Stop]", got)
	}
	if got := methods(wg.calls); !sliceEqual(got, []string{"Up", "Down"}) {
		t.Errorf("wg calls = %v, want [Up Down]", got)
	}

	if m.Phase() != PhaseStopped {
		t.Errorf("phase = %s, want stopped", m.Phase())
	}
}

func TestDestroy_ReturnsFirstError_ContinuesTeardown(t *testing.T) {
	storeErr := errors.New("store boom")
	wgErr := errors.New("wg boom")

	wg := &fakeWG{downErr: wgErr}
	store := &fakeStore{stopErr: storeErr}
	conv := &fakeConvergence{}

	m := New(WithWireGuard(wg), WithStore(store), WithConvergence(conv))

	if err := m.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	err := m.Destroy(context.Background())
	if err == nil {
		t.Fatal("Destroy should return an error")
	}

	// First error wins (store fails before WG).
	if !errors.Is(err, storeErr) {
		t.Errorf("got %v, want store error", err)
	}

	// WG.Down must still be called despite store error.
	if got := methods(wg.calls); !sliceEqual(got, []string{"Up", "Down"}) {
		t.Errorf("wg calls = %v, want [Up Down] (should continue teardown)", got)
	}

	if m.Phase() != PhaseStopped {
		t.Errorf("phase = %s, want stopped", m.Phase())
	}
}

func TestStart_ErrorDoesNotRollBack(t *testing.T) {
	wg := &fakeWG{}
	store := &fakeStore{startErr: errors.New("store connect failed")}
	conv := &fakeConvergence{}

	m := New(WithWireGuard(wg), WithStore(store), WithConvergence(conv))

	err := m.Start(context.Background())
	if err == nil {
		t.Fatal("Start should return an error")
	}

	// WireGuard.Up was called but Down must NOT be called on failure.
	if got := methods(wg.calls); !sliceEqual(got, []string{"Up"}) {
		t.Errorf("wg calls = %v, want [Up] (no rollback Down)", got)
	}

	// Store.Stop must NOT be called.
	if got := methods(store.calls); !sliceEqual(got, []string{"Start"}) {
		t.Errorf("store calls = %v, want [Start] (no rollback Stop)", got)
	}

	if m.Phase() != PhaseStopped {
		t.Errorf("phase = %s, want stopped", m.Phase())
	}
}
