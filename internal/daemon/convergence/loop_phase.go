package convergence

import "ployz/internal/support/check"

type LoopPhase uint8

const (
	LoopInitializing LoopPhase = iota + 1
	LoopSubscribed
	LoopReconciling
	LoopResubscribing
	LoopTerminated
)

func (p LoopPhase) String() string {
	switch p {
	case LoopInitializing:
		return "initializing"
	case LoopSubscribed:
		return "subscribed"
	case LoopReconciling:
		return "reconciling"
	case LoopResubscribing:
		return "resubscribing"
	case LoopTerminated:
		return "terminated"
	default:
		return "unknown"
	}
}

func (p LoopPhase) Transition(to LoopPhase) LoopPhase {
	ok := false
	switch p {
	case LoopInitializing:
		ok = to == LoopSubscribed || to == LoopTerminated
	case LoopSubscribed:
		ok = to == LoopReconciling || to == LoopResubscribing || to == LoopTerminated
	case LoopReconciling:
		ok = to == LoopSubscribed || to == LoopResubscribing || to == LoopTerminated
	case LoopResubscribing:
		ok = to == LoopSubscribed || to == LoopTerminated
	case LoopTerminated:
		ok = false
	}
	check.Assertf(ok, "supervisor loop transition: %s -> %s", p, to)
	if !ok {
		return p
	}
	return to
}
