package engine

import "ployz/internal/check"

type WorkerPhase uint8

const (
	WorkerAbsent WorkerPhase = iota + 1
	WorkerStarting
	WorkerRunning
	WorkerDegraded
	WorkerBackoff
	WorkerGivingUp
	WorkerStopping
)

func (p WorkerPhase) String() string {
	switch p {
	case WorkerAbsent:
		return "absent"
	case WorkerStarting:
		return "starting"
	case WorkerRunning:
		return "running"
	case WorkerDegraded:
		return "degraded"
	case WorkerBackoff:
		return "backoff"
	case WorkerGivingUp:
		return "giving_up"
	case WorkerStopping:
		return "stopping"
	default:
		return "unknown"
	}
}

func (p WorkerPhase) Transition(to WorkerPhase) WorkerPhase {
	ok := false
	switch p {
	case WorkerAbsent:
		ok = to == WorkerStarting
	case WorkerStarting:
		ok = to == WorkerRunning || to == WorkerBackoff || to == WorkerStopping
	case WorkerRunning:
		ok = to == WorkerDegraded || to == WorkerStopping
	case WorkerDegraded:
		ok = to == WorkerRunning || to == WorkerBackoff || to == WorkerStopping
	case WorkerBackoff:
		ok = to == WorkerStarting || to == WorkerGivingUp || to == WorkerStopping
	case WorkerGivingUp:
		ok = false
	case WorkerStopping:
		ok = to == WorkerAbsent
	}
	check.Assertf(ok, "worker phase transition: %s -> %s", p, to)
	if !ok {
		return p
	}
	return to
}
