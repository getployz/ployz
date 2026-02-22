package reconcile

import (
	"sync"
	"time"

	"ployz/internal/network"
)

const defaultStaleAge = 3 * time.Second

type peerState struct {
	lastSeen       time.Time // monotonic
	reportedAt     time.Time // wall clock from remote heartbeat
	localClockAtRx time.Time // local wall clock when we received the heartbeat
}

type PeerHealth struct {
	Freshness      time.Duration
	Stale          bool
	ReplicationLag time.Duration
	PingRTT        time.Duration // -1 = unreachable, 0 = no data
}

type FreshnessTracker struct {
	mu       sync.RWMutex
	peers    map[string]peerState
	selfID   string
	staleAge time.Duration
	clock    network.Clock
}

func NewFreshnessTracker(selfID string, clock network.Clock) *FreshnessTracker {
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
		out[id] = PeerHealth{
			Freshness:      freshness,
			Stale:          freshness > ft.staleAge,
			ReplicationLag: lag,
		}
	}
	return out
}
