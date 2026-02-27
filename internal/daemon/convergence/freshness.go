package convergence

import (
	"sync"
	"time"

	"ployz/internal/support/check"
	"ployz/internal/daemon/overlay"
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
	PingPhase      PingPhase
	PingRTT        time.Duration
}

type FreshnessTracker struct {
	mu       sync.RWMutex
	peers    map[string]peerState
	selfID   string
	staleAge time.Duration
	clock    overlay.Clock
}

func NewFreshnessTracker(selfID string, clock overlay.Clock) *FreshnessTracker {
	check.Assert(clock != nil, "freshness.NewTracker: clock must not be nil")
	return &FreshnessTracker{
		peers:    make(map[string]peerState),
		selfID:   selfID,
		staleAge: defaultStaleAge,
		clock:    clock,
	}
}

func (ft *FreshnessTracker) RecordSeen(nodeID string, updatedAt time.Time) {
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

func (ft *FreshnessTracker) Remove(nodeID string) {
	ft.mu.Lock()
	delete(ft.peers, nodeID)
	ft.mu.Unlock()
}

func (ft *FreshnessTracker) Snapshot() map[string]PeerHealth {
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
			PingPhase:      PingNoData,
		}
	}
	return out
}
