package freshness

import (
	"sync"
	"time"

	"ployz/internal/check"
	"ployz/internal/network"
	"ployz/internal/signal/ping"
)

const defaultStaleAge = 3 * time.Second

type FreshnessPhase uint8

const (
	FreshnessUnknown FreshnessPhase = iota + 1
	FreshnessFresh
	FreshnessStale
	FreshnessRemoved
)

func (p FreshnessPhase) String() string {
	switch p {
	case FreshnessUnknown:
		return "unknown"
	case FreshnessFresh:
		return "fresh"
	case FreshnessStale:
		return "stale"
	case FreshnessRemoved:
		return "removed"
	default:
		return "unknown_phase"
	}
}

func (p FreshnessPhase) Transition(to FreshnessPhase) FreshnessPhase {
	ok := false
	switch p {
	case FreshnessUnknown:
		ok = to == FreshnessFresh || to == FreshnessStale || to == FreshnessRemoved
	case FreshnessFresh:
		ok = to == FreshnessStale || to == FreshnessRemoved
	case FreshnessStale:
		ok = to == FreshnessFresh || to == FreshnessRemoved
	case FreshnessRemoved:
		ok = to == FreshnessFresh
	}
	check.Assertf(ok, "freshness transition: %s -> %s", p, to)
	if !ok {
		return p
	}
	return to
}

type peerState struct {
	lastSeen       time.Time
	reportedAt     time.Time
	localClockAtRx time.Time
}

type PeerHealth struct {
	Freshness      time.Duration
	Phase          FreshnessPhase
	ReplicationLag time.Duration
	PingPhase      ping.PingPhase
	PingRTT        time.Duration
}

type Tracker struct {
	mu       sync.RWMutex
	peers    map[string]peerState
	selfID   string
	staleAge time.Duration
	clock    network.Clock
}

func NewTracker(selfID string, clock network.Clock) *Tracker {
	check.Assert(clock != nil, "freshness.NewTracker: clock must not be nil")
	return &Tracker{
		peers:    make(map[string]peerState),
		selfID:   selfID,
		staleAge: defaultStaleAge,
		clock:    clock,
	}
}

func (ft *Tracker) RecordSeen(nodeID string, updatedAt time.Time) {
	if nodeID == ft.selfID {
		return
	}

	now := ft.clock.Now()

	ft.mu.Lock()
	ft.peers[nodeID] = peerState{
		lastSeen:       now,
		reportedAt:     updatedAt,
		localClockAtRx: now,
	}
	ft.mu.Unlock()
}

func (ft *Tracker) Remove(nodeID string) {
	ft.mu.Lock()
	delete(ft.peers, nodeID)
	ft.mu.Unlock()
}

func (ft *Tracker) Snapshot() map[string]PeerHealth {
	ft.mu.RLock()
	defer ft.mu.RUnlock()

	now := ft.clock.Now()
	out := make(map[string]PeerHealth, len(ft.peers))
	for id, p := range ft.peers {
		freshness := now.Sub(p.lastSeen)
		lag := p.localClockAtRx.Sub(p.reportedAt)
		if lag < 0 {
			lag = 0
		}
		phase := FreshnessFresh
		if freshness > ft.staleAge {
			phase = FreshnessStale
		}
		out[id] = PeerHealth{
			Freshness:      freshness,
			Phase:          phase,
			ReplicationLag: lag,
			PingPhase:      ping.PingNoData,
		}
	}
	return out
}
