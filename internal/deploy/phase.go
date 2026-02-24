package deploy

import (
	"encoding/json"
	"fmt"
	"strings"

	"ployz/internal/check"
)

type DeployPhase uint8

const (
	DeployInProgress DeployPhase = iota + 1
	DeploySucceeded
	DeployFailed
)

func (p DeployPhase) String() string {
	switch p {
	case DeployInProgress:
		return "in_progress"
	case DeploySucceeded:
		return "succeeded"
	case DeployFailed:
		return "failed"
	default:
		return "unknown"
	}
}

func (p DeployPhase) IsValid() bool {
	switch p {
	case DeployInProgress, DeploySucceeded, DeployFailed:
		return true
	default:
		return false
	}
}

func (p DeployPhase) Transition(to DeployPhase) DeployPhase {
	ok := false
	switch p {
	case DeployInProgress:
		ok = to == DeploySucceeded || to == DeployFailed
	case DeploySucceeded:
		ok = false
	case DeployFailed:
		ok = to == DeployInProgress
	}
	check.Assertf(ok, "deploy phase transition: %s -> %s", p, to)
	if !ok {
		return p
	}
	return to
}

func (p DeployPhase) MarshalJSON() ([]byte, error) {
	if !p.IsValid() {
		return nil, fmt.Errorf("invalid deploy phase: %d", p)
	}
	return json.Marshal(p.String())
}

func (p *DeployPhase) UnmarshalJSON(data []byte) error {
	var raw string
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	next, ok := parseDeployPhase(raw)
	if !ok {
		return fmt.Errorf("invalid deploy phase: %q", raw)
	}
	*p = next
	return nil
}

func parseDeployPhase(raw string) (DeployPhase, bool) {
	switch strings.TrimSpace(raw) {
	case "in_progress":
		return DeployInProgress, true
	case "succeeded":
		return DeploySucceeded, true
	case "failed":
		return DeployFailed, true
	default:
		return 0, false
	}
}

func ParseDeployPhase(raw string) (DeployPhase, bool) {
	return parseDeployPhase(raw)
}

type TierPhase uint8

const (
	TierPending TierPhase = iota + 1
	TierExecuting
	TierHealthChecking
	TierPostcondition
	TierCompleted
	TierFailed
	TierRolledBack
)

func (p TierPhase) String() string {
	switch p {
	case TierPending:
		return "pending"
	case TierExecuting:
		return "executing"
	case TierHealthChecking:
		return "health"
	case TierPostcondition:
		return "postcondition"
	case TierCompleted:
		return "completed"
	case TierFailed:
		return "failed"
	case TierRolledBack:
		return "rolled_back"
	default:
		return "unknown"
	}
}

func (p TierPhase) IsValid() bool {
	switch p {
	case TierPending, TierExecuting, TierHealthChecking, TierPostcondition, TierCompleted, TierFailed, TierRolledBack:
		return true
	default:
		return false
	}
}

func (p TierPhase) Transition(to TierPhase) TierPhase {
	ok := false
	switch p {
	case TierPending:
		ok = to == TierExecuting || to == TierFailed
	case TierExecuting:
		ok = to == TierHealthChecking || to == TierPostcondition || to == TierFailed || to == TierRolledBack
	case TierHealthChecking:
		ok = to == TierPostcondition || to == TierFailed || to == TierRolledBack
	case TierPostcondition:
		ok = to == TierCompleted || to == TierFailed
	case TierCompleted, TierFailed, TierRolledBack:
		ok = false
	}
	check.Assertf(ok, "tier phase transition: %s -> %s", p, to)
	if !ok {
		return p
	}
	return to
}

func (p TierPhase) MarshalJSON() ([]byte, error) {
	if !p.IsValid() {
		return nil, fmt.Errorf("invalid tier phase: %d", p)
	}
	return json.Marshal(p.String())
}

func (p *TierPhase) UnmarshalJSON(data []byte) error {
	var raw string
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	next, ok := parseTierPhase(raw)
	if !ok {
		return fmt.Errorf("invalid tier phase: %q", raw)
	}
	*p = next
	return nil
}

func parseTierPhase(raw string) (TierPhase, bool) {
	switch strings.TrimSpace(raw) {
	case "pending":
		return TierPending, true
	case "executing":
		return TierExecuting, true
	case "health":
		return TierHealthChecking, true
	case "postcondition":
		return TierPostcondition, true
	case "completed":
		return TierCompleted, true
	case "failed":
		return TierFailed, true
	case "rolled_back":
		return TierRolledBack, true
	default:
		return 0, false
	}
}

func ParseTierPhase(raw string) (TierPhase, bool) {
	return parseTierPhase(raw)
}

type OwnershipPhase uint8

const (
	OwnershipUnknown OwnershipPhase = iota + 1
	OwnershipHeld
	OwnershipLost
)

func (p OwnershipPhase) String() string {
	switch p {
	case OwnershipUnknown:
		return "unknown"
	case OwnershipHeld:
		return "held"
	case OwnershipLost:
		return "lost"
	default:
		return "unknown_phase"
	}
}

func (p OwnershipPhase) Transition(to OwnershipPhase) OwnershipPhase {
	ok := false
	switch p {
	case OwnershipUnknown:
		ok = to == OwnershipHeld || to == OwnershipLost
	case OwnershipHeld:
		ok = to == OwnershipLost
	case OwnershipLost:
		ok = false
	}
	check.Assertf(ok, "ownership transition: %s -> %s", p, to)
	if !ok {
		return p
	}
	return to
}

type DeployErrorPhase uint8

const (
	DeployErrorPhaseOwnership DeployErrorPhase = iota + 1
	DeployErrorPhasePrePull
	DeployErrorPhaseExecute
	DeployErrorPhaseHealth
	DeployErrorPhasePostcondition
)

func (p DeployErrorPhase) String() string {
	switch p {
	case DeployErrorPhaseOwnership:
		return "ownership"
	case DeployErrorPhasePrePull:
		return "pre-pull"
	case DeployErrorPhaseExecute:
		return "execute"
	case DeployErrorPhaseHealth:
		return "health"
	case DeployErrorPhasePostcondition:
		return "postcondition"
	default:
		return "unknown"
	}
}

func (p DeployErrorPhase) IsValid() bool {
	switch p {
	case DeployErrorPhaseOwnership,
		DeployErrorPhasePrePull,
		DeployErrorPhaseExecute,
		DeployErrorPhaseHealth,
		DeployErrorPhasePostcondition:
		return true
	default:
		return false
	}
}
