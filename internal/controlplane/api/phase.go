package api

import "ployz/internal/check"

type ServePhase uint8

const (
	ServeStartingInternal ServePhase = iota + 1
	ServeStartingProxy
	ServeWaitingForIdentity
	ServeServing
	ServeShuttingDown
)

func (p ServePhase) String() string {
	switch p {
	case ServeStartingInternal:
		return "starting_internal"
	case ServeStartingProxy:
		return "starting_proxy"
	case ServeWaitingForIdentity:
		return "waiting_for_identity"
	case ServeServing:
		return "serving"
	case ServeShuttingDown:
		return "shutting_down"
	default:
		return "unknown"
	}
}

func (p ServePhase) Transition(to ServePhase) ServePhase {
	ok := false
	switch p {
	case ServeStartingInternal:
		ok = to == ServeStartingProxy || to == ServeShuttingDown
	case ServeStartingProxy:
		ok = to == ServeWaitingForIdentity || to == ServeServing || to == ServeShuttingDown
	case ServeWaitingForIdentity:
		ok = to == ServeServing || to == ServeShuttingDown
	case ServeServing:
		ok = to == ServeShuttingDown
	case ServeShuttingDown:
		ok = false
	}
	check.Assertf(ok, "server serve transition: %s -> %s", p, to)
	if !ok {
		return p
	}
	return to
}
