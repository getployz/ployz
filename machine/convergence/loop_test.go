package convergence

import (
	"context"
	"net/netip"
	"testing"
	"testing/synctest"
	"time"

	"ployz"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

const probeTestBuffer = 250 * time.Millisecond

// --- fakes ---

type fakeProber struct {
	handshakes map[wgtypes.Key]time.Time
	err        error
}

func (f *fakeProber) PeerHandshakes(context.Context) (map[wgtypes.Key]time.Time, error) {
	return f.handshakes, f.err
}

type fakePeerSetter struct {
	lastPeers []ployz.MachineRecord
	err       error
	calls     int
}

func (f *fakePeerSetter) SetPeers(_ context.Context, peers []ployz.MachineRecord) error {
	f.lastPeers = peers
	f.calls++
	return f.err
}

type fakeSubscriber struct {
	records []ployz.MachineRecord
	events  chan ployz.MachineEvent
}

func (f *fakeSubscriber) SubscribeMachines(context.Context) ([]ployz.MachineRecord, <-chan ployz.MachineEvent, error) {
	return f.records, f.events, nil
}

// --- helpers ---

func mustKey(t *testing.T) wgtypes.Key {
	t.Helper()
	k, err := wgtypes.GeneratePrivateKey()
	if err != nil {
		t.Fatal(err)
	}
	return k.PublicKey()
}

func ep(s string) netip.AddrPort {
	return netip.MustParseAddrPort(s)
}

// loopFixture bundles common test state for convergence loop tests.
type loopFixture struct {
	loop    *Loop
	self    ployz.MachineRecord
	peer    ployz.MachineRecord
	peerKey wgtypes.Key
	records []ployz.MachineRecord
	setter  *fakePeerSetter
	prober  *fakeProber
	sub     *fakeSubscriber
}

// newFixture creates a self + single peer with the given endpoints, wires all
// fakes, and constructs a Loop with the options pattern. Tests that need custom
// config can override fields after calling newFixture.
func newFixture(t *testing.T, endpoints ...string) *loopFixture {
	t.Helper()

	selfKey := mustKey(t)
	peerKey := mustKey(t)

	self := ployz.MachineRecord{ID: "self", PublicKey: selfKey}
	peer := ployz.MachineRecord{ID: "peer1", PublicKey: peerKey}
	for _, e := range endpoints {
		peer.Endpoints = append(peer.Endpoints, ep(e))
	}

	records := []ployz.MachineRecord{self, peer}
	setter := &fakePeerSetter{}
	prober := &fakeProber{handshakes: map[wgtypes.Key]time.Time{}}
	sub := &fakeSubscriber{records: records, events: make(chan ployz.MachineEvent)}

	l := New(self,
		WithSubscriber(sub),
		WithPeerSetter(setter),
		WithProber(prober),
	)

	return &loopFixture{
		loop:    l,
		self:    self,
		peer:    peer,
		peerKey: peerKey,
		records: records,
		setter:  setter,
		prober:  prober,
		sub:     sub,
	}
}

// --- tests ---

func TestLoop_HealthUninitializedBeforeProbe(t *testing.T) {
	f := newFixture(t, "10.0.0.2:51820")

	h := f.loop.Health()
	if h.Initialized {
		t.Error("health should not be initialized before first probe")
	}
}

func TestLoop_ProbeClassifiesPeers(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		f := newFixture(t, "10.0.0.2:51820")
		f.prober.handshakes[f.peerKey] = time.Now().Add(-30 * time.Second)

		ctx, cancel := context.WithCancel(context.Background())
		if err := f.loop.Start(ctx); err != nil {
			t.Fatal(err)
		}

		// Wait for at least one probe tick + some buffer in fake time.
		time.Sleep(probeInterval + probeTestBuffer)
		synctest.Wait()

		h := f.loop.Health()
		if !h.Initialized {
			t.Fatal("health should be initialized after probe")
		}
		if h.Alive != 1 {
			t.Errorf("alive = %d, want 1", h.Alive)
		}
		if h.Total != 1 {
			t.Errorf("total = %d, want 1", h.Total)
		}

		cancel()
		f.loop.Stop()
	})
}

func TestLoop_ProberErrorDoesNotCrash(t *testing.T) {
	f := newFixture(t, "10.0.0.2:51820")
	f.prober.err = context.DeadlineExceeded

	// Directly call probe — should not panic.
	f.loop.probe(context.Background(), f.records)

	h := f.loop.Health()
	if h.Initialized {
		t.Error("health should not be initialized on prober error")
	}
}

func TestLoop_EndpointRotation(t *testing.T) {
	f := newFixture(t, "10.0.0.2:51820", "1.2.3.4:51820", "5.6.7.8:51820")

	// Initial reconcile to populate state.
	if err := f.loop.reconcile(context.Background(), f.records); err != nil {
		t.Fatal(err)
	}

	// First probe — creates peer state with endpointSetAt = now.
	f.loop.probe(context.Background(), f.records)

	// Simulate time passing beyond endpointTimeout.
	st := f.loop.peerStates[f.peerKey]
	st.endpointSetAt = time.Now().Add(-(endpointTimeout + 5*time.Second))

	setterCalls := f.setter.calls
	f.loop.probe(context.Background(), f.records)

	// Should have rotated and called SetPeers.
	if f.setter.calls <= setterCalls {
		t.Error("expected SetPeers call after rotation")
	}

	// Verify endpoint index advanced.
	if st.endpointIndex == 0 {
		t.Error("endpoint index should have advanced from 0")
	}
}

func TestLoop_NoRotationSingleEndpoint(t *testing.T) {
	f := newFixture(t, "10.0.0.2:51820")

	if err := f.loop.reconcile(context.Background(), f.records); err != nil {
		t.Fatal(err)
	}
	f.loop.probe(context.Background(), f.records)

	// Force endpointSetAt into the past.
	st := f.loop.peerStates[f.peerKey]
	st.endpointSetAt = time.Now().Add(-(endpointTimeout + 5*time.Second))

	setterCalls := f.setter.calls
	f.loop.probe(context.Background(), f.records)

	// Single-endpoint peer should NOT trigger extra SetPeers for rotation.
	if f.setter.calls != setterCalls {
		t.Error("single-endpoint peer should not trigger rotation SetPeers")
	}
}

func TestLoop_SingleEndpointTransitionsToSuspectAfterTimeout(t *testing.T) {
	f := newFixture(t, "10.0.0.2:51820")

	// First probe: peer starts in New while first attempt is in-flight.
	f.loop.probe(context.Background(), f.records)
	h := f.loop.Health()
	if h.New != 1 {
		t.Fatalf("new = %d, want 1 after first probe", h.New)
	}

	// Age the first (and only) endpoint attempt past timeout.
	st := f.loop.peerStates[f.peerKey]
	st.endpointSetAt = time.Now().Add(-(endpointTimeout + 5*time.Second))

	f.loop.probe(context.Background(), f.records)
	h = f.loop.Health()
	if h.Suspect != 1 {
		t.Fatalf("suspect = %d, want 1 after endpoint timeout", h.Suspect)
	}
	if h.New != 0 {
		t.Fatalf("new = %d, want 0 after endpoint timeout", h.New)
	}
}

func TestLoop_ReconcileKeepsRotatedEndpointSticky(t *testing.T) {
	f := newFixture(t, "198.51.100.10:51820", "203.0.113.20:51820")

	// Seed peer state as if failover previously selected endpoint index 1.
	f.loop.peerStates[f.peerKey] = &peerState{
		endpointIndex: 1,
		endpointCount: 2,
	}

	if err := f.loop.reconcile(context.Background(), f.records); err != nil {
		t.Fatalf("reconcile: %v", err)
	}

	if len(f.setter.lastPeers) != 1 {
		t.Fatalf("lastPeers len = %d, want 1", len(f.setter.lastPeers))
	}
	got := f.setter.lastPeers[0].Endpoints[0]
	want := f.peer.Endpoints[1]
	if got != want {
		t.Fatalf("active endpoint = %s, want %s", got, want)
	}
}

func TestLoop_HandshakeRecoveryResetsToAlive(t *testing.T) {
	f := newFixture(t, "10.0.0.2:51820", "1.2.3.4:51820")

	if err := f.loop.reconcile(context.Background(), f.records); err != nil {
		t.Fatal(err)
	}

	// First probe — no handshake, peer is New.
	f.loop.probe(context.Background(), f.records)
	h := f.loop.Health()
	if h.Alive != 0 {
		t.Errorf("alive = %d, want 0 (no handshake)", h.Alive)
	}

	// Now simulate a handshake.
	f.prober.handshakes[f.peerKey] = time.Now()
	f.loop.probe(context.Background(), f.records)

	h = f.loop.Health()
	if h.Alive != 1 {
		t.Errorf("alive = %d, want 1 after handshake", h.Alive)
	}
}

func TestLoop_NilProberSkipsProbing(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		selfKey := mustKey(t)
		self := ployz.MachineRecord{ID: "self", PublicKey: selfKey}

		sub := &fakeSubscriber{
			records: []ployz.MachineRecord{self},
			events:  make(chan ployz.MachineEvent),
		}
		setter := &fakePeerSetter{}

		l := New(self,
			WithSubscriber(sub),
			WithPeerSetter(setter),
		)

		ctx, cancel := context.WithCancel(context.Background())
		if err := l.Start(ctx); err != nil {
			t.Fatal(err)
		}

		// Let it run briefly in fake time — should not panic.
		time.Sleep(100 * time.Millisecond)
		synctest.Wait()

		h := l.Health()
		if h.Initialized {
			t.Error("health should not be initialized without prober")
		}

		cancel()
		l.Stop()
	})
}
