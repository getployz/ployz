package machine

import "ployz/internal/check"

type AddPhase uint8

const (
	AddInstall AddPhase = iota + 1
	AddConnect
	AddConfigure
	AddRegister
	AddConverge
	AddDone
	AddFailed
)

func (p AddPhase) String() string {
	switch p {
	case AddInstall:
		return "install"
	case AddConnect:
		return "connect"
	case AddConfigure:
		return "configure"
	case AddRegister:
		return "register"
	case AddConverge:
		return "converge"
	case AddDone:
		return "done"
	case AddFailed:
		return "failed"
	default:
		return "unknown"
	}
}

func (p AddPhase) Transition(to AddPhase) AddPhase {
	ok := false
	switch p {
	case AddInstall:
		ok = to == AddConnect || to == AddFailed
	case AddConnect:
		ok = to == AddConfigure || to == AddFailed
	case AddConfigure:
		ok = to == AddRegister || to == AddFailed
	case AddRegister:
		ok = to == AddConverge || to == AddFailed
	case AddConverge:
		ok = to == AddDone || to == AddFailed
	case AddDone, AddFailed:
		ok = false
	}
	check.Assertf(ok, "machine add transition: %s -> %s", p, to)
	if !ok {
		return p
	}
	return to
}
