package machine

// Phase describes the network lifecycle state.
type Phase uint8

const (
	PhaseStopped Phase = iota
	PhaseStarting
	PhaseRunning
	PhaseStopping
	// TODO: PhaseDegraded — running but unhealthy. Add when convergence/health lands.
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
		return "unknown"
	}
}
