package engine

import "ployz/internal/check"

type SupervisorPhase uint8

const (
	SupervisorAbsent SupervisorPhase = iota + 1
	SupervisorStarting
	SupervisorRunning
	SupervisorDegraded
	SupervisorBackoff
	SupervisorGivingUp
	SupervisorStopping
)

func (p SupervisorPhase) String() string {
	switch p {
	case SupervisorAbsent:
		return "absent"
	case SupervisorStarting:
		return "starting"
	case SupervisorRunning:
		return "running"
	case SupervisorDegraded:
		return "degraded"
	case SupervisorBackoff:
		return "backoff"
	case SupervisorGivingUp:
		return "giving_up"
	case SupervisorStopping:
		return "stopping"
	default:
		return "unknown"
	}
}

func (p SupervisorPhase) Transition(to SupervisorPhase) SupervisorPhase {
	ok := false
	switch p {
	case SupervisorAbsent:
		ok = to == SupervisorStarting
	case SupervisorStarting:
		ok = to == SupervisorRunning || to == SupervisorBackoff || to == SupervisorStopping
	case SupervisorRunning:
		ok = to == SupervisorDegraded || to == SupervisorStopping
	case SupervisorDegraded:
		ok = to == SupervisorRunning || to == SupervisorBackoff || to == SupervisorStopping
	case SupervisorBackoff:
		ok = to == SupervisorStarting || to == SupervisorGivingUp || to == SupervisorStopping
	case SupervisorGivingUp:
		ok = false
	case SupervisorStopping:
		ok = to == SupervisorAbsent
	}
	check.Assertf(ok, "supervisor phase transition: %s -> %s", p, to)
	if !ok {
		return p
	}
	return to
}
