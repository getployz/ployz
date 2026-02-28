package mesh

import "ployz/internal/support/check"

// Phase describes the network lifecycle state.
type Phase uint8

const (
	PhaseStopped Phase = iota
	PhaseStarting
	PhaseRunning
	PhaseStopping
)

func (p Phase) String() string {
	switch p {
	case PhaseStopped:
		return "stopped"
	case PhaseStarting:
		return "starting"
	case PhaseRunning:
		return "running"
	case PhaseStopping:
		return "stopping"
	default:
		check.Assertf(false, "unknown mesh phase: %d", p)
		return "unknown"
	}
}
