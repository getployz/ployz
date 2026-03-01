package proxy

import (
	"context"
	"fmt"
)

// MachineInfo holds resolved machine details for proxy routing.
type MachineInfo struct {
	ID           string // WireGuard public key
	ManagementIP string // WireGuard management IPv6
	OverlayIP    string // WireGuard overlay IPv4 (first host IP in subnet)
}

// MachineMapper resolves machine IDs to management IPs for proxy routing.
type MachineMapper interface {
	// ListMachines returns all machines.
	ListMachines(ctx context.Context) ([]MachineInfo, error)
}

// MapperFunc adapts a function to the MachineMapper interface.
type MapperFunc func(ctx context.Context) ([]MachineInfo, error)

func (f MapperFunc) ListMachines(ctx context.Context) ([]MachineInfo, error) {
	return f(ctx)
}

// resolveMachines resolves machine IDs (or "*" wildcard) to targets with management IPs.
func resolveMachines(ctx context.Context, mapper MachineMapper, ids []string) ([]MachineInfo, error) {
	// Check for wildcard.
	for _, id := range ids {
		if id == "*" {
			return mapper.ListMachines(ctx)
		}
	}

	// Resolve individual IDs.
	all, err := mapper.ListMachines(ctx)
	if err != nil {
		return nil, err
	}

	byID := make(map[string]MachineInfo, len(all))
	for _, m := range all {
		byID[m.ID] = m
	}

	resolved := make([]MachineInfo, 0, len(ids))
	for _, id := range ids {
		m, ok := byID[id]
		if !ok {
			return nil, fmt.Errorf("machine not found: %s", id)
		}
		resolved = append(resolved, m)
	}
	return resolved, nil
}
