package ployz

// PeerHealth describes a peer's reachability state based on WireGuard handshakes.
type PeerHealth uint8

const (
	PeerNew     PeerHealth = iota // first sweep: trying endpoints, not all tried yet
	PeerAlive                     // fresh handshake (within aliveFreshness)
	PeerSuspect                   // all endpoints tried, no fresh handshake, still cycling
)

func (h PeerHealth) String() string {
	switch h {
	case PeerNew:
		return "new"
	case PeerAlive:
		return "alive"
	case PeerSuspect:
		return "suspect"
	default:
		return "unknown"
	}
}

// HealthSummary is a snapshot of peer health across the mesh.
type HealthSummary struct {
	Initialized bool // at least one probe cycle completed
	Total       int
	New         int
	Alive       int
	Suspect     int
}

// HasReachablePeers returns true if any peer has a confirmed handshake or
// hasn't exhausted all its endpoints yet.
func (s HealthSummary) HasReachablePeers() bool {
	return s.Alive > 0 || s.New > 0
}
