package convergence

import (
	"context"
	"net/netip"
	"testing"
	"time"

	"ployz"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

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

// --- tests ---

func TestLoop_HealthUninitializedBeforeProbe(t *testing.T) {
	self := ployz.MachineRecord{PublicKey: mustKey(t)}
	sub := &fakeSubscriber{events: make(chan ployz.MachineEvent)}
	setter := &fakePeerSetter{}
	prober := &fakeProber{handshakes: map[wgtypes.Key]time.Time{}}

	l := New(self, MeshPlanner{}, sub, setter, prober)

	h := l.Health()
	if h.Initialized {
		t.Error("health should not be initialized before first probe")
	}
}

func TestLoop_ProbeClassifiesPeers(t *testing.T) {
	selfKey := mustKey(t)
	peerKey := mustKey(t)

	self := ployz.MachineRecord{ID: "self", PublicKey: selfKey}
	peer := ployz.MachineRecord{
		ID:        "peer1",
		PublicKey: peerKey,
		Endpoints: []netip.AddrPort{ep("10.0.0.2:51820")},
	}

	sub := &fakeSubscriber{
		records: []ployz.MachineRecord{self, peer},
		events:  make(chan ployz.MachineEvent),
	}
	setter := &fakePeerSetter{}
	prober := &fakeProber{
		handshakes: map[wgtypes.Key]time.Time{
			peerKey: time.Now().Add(-30 * time.Second),
		},
	}

	l := New(self, MeshPlanner{}, sub, setter, prober)

	ctx, cancel := context.WithCancel(context.Background())
	if err := l.Start(ctx); err != nil {
		t.Fatal(err)
	}

	// Wait for at least one probe tick (5s) + some buffer.
	time.Sleep(6 * time.Second)

	h := l.Health()
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
	l.Stop()
}

func TestLoop_ProberErrorDoesNotCrash(t *testing.T) {
	selfKey := mustKey(t)
	peerKey := mustKey(t)

	self := ployz.MachineRecord{ID: "self", PublicKey: selfKey}
	peer := ployz.MachineRecord{
		ID:        "peer1",
		PublicKey: peerKey,
		Endpoints: []netip.AddrPort{ep("10.0.0.2:51820")},
	}

	records := []ployz.MachineRecord{self, peer}

	sub := &fakeSubscriber{records: records, events: make(chan ployz.MachineEvent)}
	setter := &fakePeerSetter{}
	prober := &fakeProber{err: context.DeadlineExceeded}

	l := New(self, MeshPlanner{}, sub, setter, prober)

	// Directly call probe — should not panic.
	l.probe(context.Background(), records)

	h := l.Health()
	if h.Initialized {
		t.Error("health should not be initialized on prober error")
	}
}

func TestLoop_EndpointRotation(t *testing.T) {
	selfKey := mustKey(t)
	peerKey := mustKey(t)

	self := ployz.MachineRecord{ID: "self", PublicKey: selfKey}
	peer := ployz.MachineRecord{
		ID:        "peer1",
		PublicKey: peerKey,
		Endpoints: []netip.AddrPort{
			ep("10.0.0.2:51820"),
			ep("1.2.3.4:51820"),
			ep("5.6.7.8:51820"),
		},
	}

	records := []ployz.MachineRecord{self, peer}
	setter := &fakePeerSetter{}
	prober := &fakeProber{handshakes: map[wgtypes.Key]time.Time{}}
	sub := &fakeSubscriber{records: records, events: make(chan ployz.MachineEvent)}

	l := New(self, MeshPlanner{}, sub, setter, prober)

	// Initial reconcile to populate state.
	if err := l.reconcile(context.Background(), records, nil); err != nil {
		t.Fatal(err)
	}

	// First probe — creates peer state with endpointSetAt = now.
	l.probe(context.Background(), records)

	// Simulate time passing beyond endpointTimeout.
	st := l.peerStates[peerKey]
	st.endpointSetAt = time.Now().Add(-20 * time.Second)

	setterCalls := setter.calls
	l.probe(context.Background(), records)

	// Should have rotated and called SetPeers.
	if setter.calls <= setterCalls {
		t.Error("expected SetPeers call after rotation")
	}

	// Verify endpoint index advanced.
	if st.endpointIndex == 0 {
		t.Error("endpoint index should have advanced from 0")
	}
}

func TestLoop_NoRotationSingleEndpoint(t *testing.T) {
	selfKey := mustKey(t)
	peerKey := mustKey(t)

	self := ployz.MachineRecord{ID: "self", PublicKey: selfKey}
	peer := ployz.MachineRecord{
		ID:        "peer1",
		PublicKey: peerKey,
		Endpoints: []netip.AddrPort{ep("10.0.0.2:51820")},
	}

	records := []ployz.MachineRecord{self, peer}
	setter := &fakePeerSetter{}
	prober := &fakeProber{handshakes: map[wgtypes.Key]time.Time{}}
	sub := &fakeSubscriber{records: records, events: make(chan ployz.MachineEvent)}

	l := New(self, MeshPlanner{}, sub, setter, prober)
	if err := l.reconcile(context.Background(), records, nil); err != nil {
		t.Fatal(err)
	}

	l.probe(context.Background(), records)

	// Force endpointSetAt into the past.
	st := l.peerStates[peerKey]
	st.endpointSetAt = time.Now().Add(-20 * time.Second)

	setterCalls := setter.calls
	l.probe(context.Background(), records)

	// Single-endpoint peer should NOT trigger extra SetPeers for rotation.
	// (probe itself doesn't call SetPeers unless rotation happens)
	if setter.calls != setterCalls {
		t.Error("single-endpoint peer should not trigger rotation SetPeers")
	}
}

func TestLoop_HandshakeRecoveryResetsToAlive(t *testing.T) {
	selfKey := mustKey(t)
	peerKey := mustKey(t)

	self := ployz.MachineRecord{ID: "self", PublicKey: selfKey}
	peer := ployz.MachineRecord{
		ID:        "peer1",
		PublicKey: peerKey,
		Endpoints: []netip.AddrPort{
			ep("10.0.0.2:51820"),
			ep("1.2.3.4:51820"),
		},
	}

	records := []ployz.MachineRecord{self, peer}
	setter := &fakePeerSetter{}
	prober := &fakeProber{handshakes: map[wgtypes.Key]time.Time{}}
	sub := &fakeSubscriber{records: records, events: make(chan ployz.MachineEvent)}

	l := New(self, MeshPlanner{}, sub, setter, prober)
	if err := l.reconcile(context.Background(), records, nil); err != nil {
		t.Fatal(err)
	}

	// First probe — no handshake, peer is New.
	l.probe(context.Background(), records)
	h := l.Health()
	if h.Alive != 0 {
		t.Errorf("alive = %d, want 0 (no handshake)", h.Alive)
	}

	// Now simulate a handshake.
	prober.handshakes[peerKey] = time.Now()
	l.probe(context.Background(), records)

	h = l.Health()
	if h.Alive != 1 {
		t.Errorf("alive = %d, want 1 after handshake", h.Alive)
	}
}

func TestLoop_NilProberSkipsProbing(t *testing.T) {
	selfKey := mustKey(t)
	self := ployz.MachineRecord{ID: "self", PublicKey: selfKey}

	sub := &fakeSubscriber{
		records: []ployz.MachineRecord{self},
		events:  make(chan ployz.MachineEvent),
	}
	setter := &fakePeerSetter{}

	l := New(self, MeshPlanner{}, sub, setter, nil)

	ctx, cancel := context.WithCancel(context.Background())
	if err := l.Start(ctx); err != nil {
		t.Fatal(err)
	}

	// Let it run briefly — should not panic.
	time.Sleep(100 * time.Millisecond)

	h := l.Health()
	if h.Initialized {
		t.Error("health should not be initialized without prober")
	}

	cancel()
	l.Stop()
}
