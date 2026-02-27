package container

import "ployz/internal/support/check"

type ContainerPhase uint8

const (
	ContainerNotPresent ContainerPhase = iota + 1
	ContainerRemovingStale
	ContainerCreating
	ContainerPullingImage
	ContainerStarting
	ContainerWaitingReady
	ContainerApplyingSchema
	ContainerOperational
)

func (p ContainerPhase) String() string {
	switch p {
	case ContainerNotPresent:
		return "not_present"
	case ContainerRemovingStale:
		return "removing_stale"
	case ContainerCreating:
		return "creating"
	case ContainerPullingImage:
		return "pulling_image"
	case ContainerStarting:
		return "starting"
	case ContainerWaitingReady:
		return "waiting_ready"
	case ContainerApplyingSchema:
		return "applying_schema"
	case ContainerOperational:
		return "operational"
	default:
		return "unknown"
	}
}

func (p ContainerPhase) Transition(to ContainerPhase) ContainerPhase {
	ok := false
	switch p {
	case ContainerNotPresent:
		ok = to == ContainerRemovingStale || to == ContainerCreating
	case ContainerRemovingStale:
		ok = to == ContainerCreating
	case ContainerCreating:
		ok = to == ContainerStarting || to == ContainerPullingImage
	case ContainerPullingImage:
		ok = to == ContainerCreating
	case ContainerStarting:
		ok = to == ContainerWaitingReady
	case ContainerWaitingReady:
		ok = to == ContainerApplyingSchema
	case ContainerApplyingSchema:
		ok = to == ContainerOperational
	case ContainerOperational:
		ok = false
	}
	check.Assertf(ok, "corrosion container transition: %s -> %s", p, to)
	if !ok {
		return p
	}
	return to
}
