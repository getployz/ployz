package network

import (
	"encoding/json"
	"fmt"
	"strings"

	"ployz/internal/check"
)

type NetworkRuntimePhase uint8

const (
	NetworkUnconfigured NetworkRuntimePhase = iota + 1
	NetworkStopped
	NetworkStarting
	NetworkRunning
	NetworkStopping
	NetworkPurged
	NetworkFailed
)

func (p NetworkRuntimePhase) String() string {
	switch p {
	case NetworkUnconfigured:
		return "unconfigured"
	case NetworkStopped:
		return "stopped"
	case NetworkStarting:
		return "starting"
	case NetworkRunning:
		return "running"
	case NetworkStopping:
		return "stopping"
	case NetworkPurged:
		return "purged"
	case NetworkFailed:
		return "failed"
	default:
		return "unknown"
	}
}

func (p NetworkRuntimePhase) Transition(to NetworkRuntimePhase) NetworkRuntimePhase {
	ok := false
	switch p {
	case NetworkUnconfigured:
		ok = to == NetworkStarting || to == NetworkPurged
	case NetworkStopped:
		ok = to == NetworkStarting || to == NetworkPurged
	case NetworkStarting:
		ok = to == NetworkRunning || to == NetworkStopping || to == NetworkFailed
	case NetworkRunning:
		ok = to == NetworkStopping || to == NetworkFailed
	case NetworkStopping:
		ok = to == NetworkStopped || to == NetworkPurged || to == NetworkFailed
	case NetworkPurged:
		ok = to == NetworkStarting
	case NetworkFailed:
		ok = to == NetworkStarting || to == NetworkStopping || to == NetworkPurged
	}
	check.Assertf(ok, "network runtime transition: %s -> %s", p, to)
	if !ok {
		return p
	}
	return to
}

func (p NetworkRuntimePhase) MarshalJSON() ([]byte, error) {
	return json.Marshal(p.String())
}

func (p *NetworkRuntimePhase) UnmarshalJSON(data []byte) error {
	var raw string
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	next, ok := parseNetworkRuntimePhase(raw)
	if !ok {
		return fmt.Errorf("invalid network runtime phase: %q", raw)
	}
	*p = next
	return nil
}

func parseNetworkRuntimePhase(raw string) (NetworkRuntimePhase, bool) {
	switch strings.TrimSpace(raw) {
	case "unconfigured":
		return NetworkUnconfigured, true
	case "stopped":
		return NetworkStopped, true
	case "starting":
		return NetworkStarting, true
	case "running":
		return NetworkRunning, true
	case "stopping":
		return NetworkStopping, true
	case "purged":
		return NetworkPurged, true
	case "failed":
		return NetworkFailed, true
	default:
		return 0, false
	}
}
