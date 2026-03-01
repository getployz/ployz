package mesh

import (
	"context"
	"errors"
	"slices"
	"strings"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"ployz"
)

const (
	testSignalDelay = bootstrapPollInterval / 40
	testPhaseDelay  = bootstrapPollInterval / 20
	testWaitTimeout = bootstrapPollInterval
)

// --- fakes ---

type fakeWG struct {
	calls   []string
	upErr   error
	downErr error
}

func (f *fakeWG) Up(context.Context) error {
	f.calls = append(f.calls, "Up")
	return f.upErr
}

func (f *fakeWG) SetPeers(context.Context, []ployz.MachineRecord) error {
	f.calls = append(f.calls, "SetPeers")
	return nil
}

func (f *fakeWG) Down(context.Context) error {
	f.calls = append(f.calls, "Down")
	return f.downErr
}

type fakeStore struct {
	calls    []string
	startErr error
	stopErr  error
}

func (f *fakeStore) Start(context.Context) error {
	f.calls = append(f.calls, "Start")
	return f.startErr
}

func (f *fakeStore) Stop(context.Context) error {
	f.calls = append(f.calls, "Stop")
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
	calls   []string
	stopErr error
	health  atomic.Pointer[ployz.HealthSummary]
}

func newFakeConvergence() *fakeConvergence {
	fc := &fakeConvergence{}
	h := ployz.HealthSummary{Initialized: true}
	fc.health.Store(&h)
	return fc
}

func (f *fakeConvergence) Start(context.Context) error {
	f.calls = append(f.calls, "Start")
	return nil
}

func (f *fakeConvergence) Stop() error {
	f.calls = append(f.calls, "Stop")
	return f.stopErr
}

func (f *fakeConvergence) Health() ployz.HealthSummary {
	return *f.health.Load()
}

func (f *fakeConvergence) setHealth(h ployz.HealthSummary) {
	f.health.Store(&h)
}

type fakeStoreHealth struct {
	healthy atomic.Bool
	err     error
}

func (f *fakeStoreHealth) Healthy(context.Context) (bool, error) {
	return f.healthy.Load(), f.err
}

// --- tests ---

func TestDetach_OnlyStopsConvergence(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		wg := &fakeWG{}
		store := &fakeStore{}
		conv := newFakeConvergence()

		m := New(WithWireGuard(wg), WithStore(store), WithConvergence(conv))

		if err := m.Up(context.Background()); err != nil {
			t.Fatalf("Up: %v", err)
		}

		if err := m.Detach(context.Background()); err != nil {
			t.Fatalf("Detach: %v", err)
		}

		// Convergence must be stopped.
		if !slices.Equal(conv.calls, []string{"Start", "Stop"}) {
			t.Errorf("convergence calls = %v, want [Start Stop]", conv.calls)
		}

		// Store and WireGuard must NOT be touched during Detach.
		if !slices.Equal(store.calls, []string{"Start"}) {
			t.Errorf("store calls = %v, want [Start] (no Stop)", store.calls)
		}
		if !slices.Equal(wg.calls, []string{"Up"}) {
			t.Errorf("wg calls = %v, want [Up] (no Down)", wg.calls)
		}

		if m.Phase() != PhaseStopped {
			t.Errorf("phase = %s, want stopped", m.Phase())
		}
	})
}

func TestDestroy_TearsDownInReverseOrder(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		wg := &fakeWG{}
		store := &fakeStore{}
		conv := newFakeConvergence()

		m := New(WithWireGuard(wg), WithStore(store), WithConvergence(conv))

		if err := m.Up(context.Background()); err != nil {
			t.Fatalf("Up: %v", err)
		}

		if err := m.Destroy(context.Background()); err != nil {
			t.Fatalf("Destroy: %v", err)
		}

		// Verify reverse order: convergence stop, store stop, WG down.
		if !slices.Equal(conv.calls, []string{"Start", "Stop"}) {
			t.Errorf("convergence calls = %v, want [Start Stop]", conv.calls)
		}
		if !slices.Equal(store.calls, []string{"Start", "Stop"}) {
			t.Errorf("store calls = %v, want [Start Stop]", store.calls)
		}
		if !slices.Equal(wg.calls, []string{"Up", "Down"}) {
			t.Errorf("wg calls = %v, want [Up Down]", wg.calls)
		}

		if m.Phase() != PhaseStopped {
			t.Errorf("phase = %s, want stopped", m.Phase())
		}
	})
}

func TestDestroy_ReturnsFirstError_ContinuesTeardown(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		storeErr := errors.New("store boom")
		wgErr := errors.New("wg boom")

		wg := &fakeWG{downErr: wgErr}
		store := &fakeStore{stopErr: storeErr}
		conv := newFakeConvergence()

		m := New(WithWireGuard(wg), WithStore(store), WithConvergence(conv))

		if err := m.Up(context.Background()); err != nil {
			t.Fatalf("Up: %v", err)
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
		if !slices.Equal(wg.calls, []string{"Up", "Down"}) {
			t.Errorf("wg calls = %v, want [Up Down] (should continue teardown)", wg.calls)
		}

		if m.Phase() != PhaseStopped {
			t.Errorf("phase = %s, want stopped", m.Phase())
		}
	})
}

func TestUp_ErrorDoesNotRollBack(t *testing.T) {
	wg := &fakeWG{}
	store := &fakeStore{startErr: errors.New("store connect failed")}
	conv := newFakeConvergence()

	m := New(WithWireGuard(wg), WithStore(store), WithConvergence(conv))

	err := m.Up(context.Background())
	if err == nil {
		t.Fatal("Up should return an error")
	}

	// WireGuard.Up was called but Down must NOT be called on failure.
	if !slices.Equal(wg.calls, []string{"Up"}) {
		t.Errorf("wg calls = %v, want [Up] (no rollback Down)", wg.calls)
	}

	// Store.Stop must NOT be called.
	if !slices.Equal(store.calls, []string{"Start"}) {
		t.Errorf("store calls = %v, want [Start] (no rollback Stop)", store.calls)
	}

	if m.Phase() != PhaseStopped {
		t.Errorf("phase = %s, want stopped", m.Phase())
	}
}

func TestDestroy_AfterPartialUp(t *testing.T) {
	wg := &fakeWG{}
	store := &fakeStore{startErr: errors.New("store connect failed")}
	conv := newFakeConvergence()

	m := New(WithWireGuard(wg), WithStore(store), WithConvergence(conv))

	// Up fails after WG is already up.
	if err := m.Up(context.Background()); err == nil {
		t.Fatal("Up should fail")
	}

	// Reset store error so Destroy's Stop call succeeds.
	store.startErr = nil

	// Destroy must still tear down WG even though phase is stopped.
	if err := m.Destroy(context.Background()); err != nil {
		t.Fatalf("Destroy: %v", err)
	}

	if !slices.Equal(wg.calls, []string{"Up", "Down"}) {
		t.Errorf("wg calls = %v, want [Up Down]", wg.calls)
	}

	if !slices.Equal(store.calls, []string{"Start", "Stop"}) {
		t.Errorf("store calls = %v, want [Start Stop]", store.calls)
	}
}

// --- bootstrap gate tests ---

func TestUp_BootstrapReachablePeers(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		conv := newFakeConvergence()
		sh := &fakeStoreHealth{}

		m := New(
			WithConvergence(conv),
			WithStoreHealth(sh),
			WithBootstrapTimeout(5*time.Second),
		)

		// Simulate: convergence reports reachable peers, store becomes healthy.
		go func() {
			time.Sleep(testSignalDelay)
			conv.setHealth(ployz.HealthSummary{Initialized: true, Total: 2, Alive: 2})
			sh.healthy.Store(true)
		}()

		if err := m.Up(context.Background()); err != nil {
			t.Fatalf("Up: %v", err)
		}

		if m.Phase() != PhaseRunning {
			t.Errorf("phase = %s, want running", m.Phase())
		}
	})
}

func TestUp_BootstrapNoPeers(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		conv := newFakeConvergence()

		m := New(
			WithConvergence(conv),
			WithBootstrapTimeout(5*time.Second),
		)

		// Simulate: no reachable peers — all suspect.
		go func() {
			time.Sleep(testSignalDelay)
			conv.setHealth(ployz.HealthSummary{Initialized: true, Total: 1, Suspect: 1})
		}()

		if err := m.Up(context.Background()); err != nil {
			t.Fatalf("Up: %v", err)
		}

		if m.Phase() != PhaseRunning {
			t.Errorf("phase = %s, want running", m.Phase())
		}
	})
}

func TestUp_BootstrapTimeout(t *testing.T) {
	conv := newFakeConvergence()
	sh := &fakeStoreHealth{}

	m := New(
		WithConvergence(conv),
		WithStoreHealth(sh),
		WithBootstrapTimeout(200*time.Millisecond),
	)

	// Convergence has reachable peers but store never becomes healthy.
	conv.setHealth(ployz.HealthSummary{Initialized: true, Total: 2, Alive: 1, New: 1})

	err := m.Up(context.Background())
	if err == nil {
		t.Fatal("Up should return a timeout error")
	}
	if !errors.Is(err, ErrBootstrapTimeout) {
		t.Fatalf("Up error = %v, want ErrBootstrapTimeout", err)
	}
}

func TestUp_BootstrapWaitsForInitialized(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		conv := newFakeConvergence()
		sh := &fakeStoreHealth{}
		conv.setHealth(ployz.HealthSummary{})

		m := New(
			WithConvergence(conv),
			WithStoreHealth(sh),
			WithBootstrapTimeout(5*time.Second),
		)

		// Simulate: uninitialized for a moment, then initialized with no peers.
		go func() {
			time.Sleep(testPhaseDelay)
			conv.setHealth(ployz.HealthSummary{Initialized: true, Total: 0})
		}()

		if err := m.Up(context.Background()); err != nil {
			t.Fatalf("Up: %v", err)
		}

		if m.Phase() != PhaseRunning {
			t.Errorf("phase = %s, want running", m.Phase())
		}
	})
}

func TestUp_BootstrapContextCanceled(t *testing.T) {
	conv := newFakeConvergence()
	conv.setHealth(ployz.HealthSummary{Initialized: true, Total: 2, Alive: 1})
	sh := &fakeStoreHealth{}

	m := New(
		WithConvergence(conv),
		WithStoreHealth(sh),
		WithBootstrapTimeout(30*time.Second),
	)

	ctx, cancel := context.WithCancel(context.Background())
	go func() {
		time.Sleep(testSignalDelay)
		cancel()
	}()

	err := m.Up(ctx)
	if err == nil {
		t.Fatal("Up should return cancellation error")
	}
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("Up error = %v, want context canceled", err)
	}
}

func TestUp_DestroyDuringBootstrap(t *testing.T) {
	conv := newFakeConvergence()
	sh := &fakeStoreHealth{}

	m := New(
		WithConvergence(conv),
		WithStoreHealth(sh),
		WithBootstrapTimeout(30*time.Second),
	)

	// Convergence never becomes ready. Up will block on bootstrap.
	conv.setHealth(ployz.HealthSummary{Initialized: true, Total: 2, Alive: 1})

	errCh := make(chan error, 1)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go func() {
		errCh <- m.Up(ctx)
	}()

	// Wait for bootstrap phase.
	time.Sleep(testPhaseDelay)
	if m.Phase() != PhaseBootstrapping {
		t.Fatalf("phase = %s, want bootstrapping", m.Phase())
	}

	// Detach should acquire lock and stop convergence.
	if err := m.Detach(context.Background()); err != nil {
		t.Fatalf("Detach: %v", err)
	}

	cancel()
	select {
	case err := <-errCh:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("Up error = %v, want context canceled", err)
		}
	case <-time.After(testWaitTimeout):
		t.Fatal("Up did not return after context cancellation")
	}
}

func TestUp_NoHealthSources(t *testing.T) {
	// nil convergence + nil storeHealth → skips gate entirely.
	m := New()

	if err := m.Up(context.Background()); err != nil {
		t.Fatalf("Up: %v", err)
	}

	if m.Phase() != PhaseRunning {
		t.Errorf("phase = %s, want running", m.Phase())
	}
}

func TestUp_ReachablePeersNoStoreHealth(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		conv := newFakeConvergence()
		conv.setHealth(ployz.HealthSummary{Initialized: true, Total: 2, Alive: 2})

		// No store health configured but convergence reports reachable peers.
		m := New(
			WithConvergence(conv),
			WithBootstrapTimeout(2*time.Second),
		)

		err := m.Up(context.Background())
		if err == nil {
			t.Fatal("Up should return an error when peers are reachable but no store health")
		}
		if !strings.Contains(err.Error(), "no store health checker") {
			t.Fatalf("Up error = %v, want missing store health message", err)
		}
	})
}
