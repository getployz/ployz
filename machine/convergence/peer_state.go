package convergence

import (
	"time"

	"ployz"
)

const (
	// endpointTimeout is how long we wait on a single endpoint before trying
	// the next one. Intentionally aggressive — WG keepalive can lag ~25s on
	// first contact, so this may mark peers Suspect early. Acceptable because
	// Suspect peers keep cycling and recover on handshake.
	endpointTimeout = 15 * time.Second

	// aliveFreshness is the maximum age of a WireGuard handshake for a peer
	// to be considered alive. From the WireGuard whitepaper: 180 + 5 + 90.
	aliveFreshness = 275 * time.Second
)

// peerState tracks the health and endpoint rotation state for a single peer.
type peerState struct {
	endpointIndex      int       // current active endpoint
	endpointSetAt      time.Time // when current endpoint was configured via SetPeers
	endpointCount      int       // total endpoints available
	endpointsAttempted int       // distinct endpoints tried since last handshake
	lastHandshake      time.Time // most recent WG handshake
	health             ployz.PeerHealth
}

// classifyPeer determines the health state of a peer based on handshake
// freshness and endpoint sweep progress, updating s.health in place.
func classifyPeer(s *peerState, now time.Time) {
	if hasFreshHandshake(s, now) {
		// Reset failure tracking but keep endpointIndex — the working
		// endpoint stays sticky.
		s.endpointsAttempted = 0
		s.health = ployz.PeerAlive
		return
	}

	// Single-endpoint peers never rotate, so we still need to mark the first
	// endpoint as "attempted" once its timeout elapses.
	if s.endpointCount == 1 && s.endpointsAttempted == 0 &&
		!s.endpointSetAt.IsZero() && now.Sub(s.endpointSetAt) >= endpointTimeout {
		s.endpointsAttempted = 1
	}

	if s.endpointsAttempted < s.endpointCount {
		s.health = ployz.PeerNew
		return
	}

	s.health = ployz.PeerSuspect
}

// hasFreshHandshake reports whether the peer has a recent WireGuard handshake.
func hasFreshHandshake(s *peerState, now time.Time) bool {
	return !s.lastHandshake.IsZero() && now.Sub(s.lastHandshake) <= aliveFreshness
}

// shouldRotate reports whether it's time to try the next endpoint.
// Returns false for single-endpoint peers.
func shouldRotate(s *peerState, now time.Time) bool {
	if s.endpointCount <= 1 {
		return false
	}
	// If we have a recent handshake, don't rotate — endpoint is working
	// or was recently working.
	if hasFreshHandshake(s, now) {
		return false
	}
	// Current endpoint has had endpointTimeout to establish a handshake.
	return !s.endpointSetAt.IsZero() && now.Sub(s.endpointSetAt) >= endpointTimeout
}

// rotationSnapshot captures the fields mutated by nextEndpoint so they
// can be restored on reconcile failure.
type rotationSnapshot struct {
	endpointIndex      int
	endpointsAttempted int
}

func snapshotRotation(s *peerState) rotationSnapshot {
	return rotationSnapshot{
		endpointIndex:      s.endpointIndex,
		endpointsAttempted: s.endpointsAttempted,
	}
}

func restoreRotation(s *peerState, snap rotationSnapshot) {
	s.endpointIndex = snap.endpointIndex
	s.endpointsAttempted = snap.endpointsAttempted
}

// nextEndpoint advances to the next endpoint index, wrapping around.
// Increments endpointsAttempted (capped at endpointCount).
func nextEndpoint(s *peerState) int {
	s.endpointIndex = (s.endpointIndex + 1) % s.endpointCount
	if s.endpointsAttempted < s.endpointCount {
		s.endpointsAttempted++
	}
	return s.endpointIndex
}
