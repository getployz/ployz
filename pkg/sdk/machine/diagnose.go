package machine

import (
	"context"

	"ployz/pkg/sdk/types"
)

// Diagnosis is a structured health assessment derived from runtime state machines.
type Diagnosis struct {
	Status               types.NetworkStatus
	ServiceBlockers      []types.StatusIssue
	ControlPlaneBlockers []types.StatusIssue
	Warnings             []types.StatusIssue
}

func (d Diagnosis) ServiceReady() bool {
	return len(d.ServiceBlockers) == 0
}

func (d Diagnosis) ControlPlaneReady() bool {
	return len(d.ControlPlaneBlockers) == 0
}

func (s *Service) Diagnose(ctx context.Context) (Diagnosis, error) {
	status, err := s.Status(ctx)
	if err != nil {
		return Diagnosis{}, err
	}
	return Diagnosis{
		Status:               status,
		ServiceBlockers:      status.ServiceBlockerIssues(),
		ControlPlaneBlockers: status.ControlPlaneBlockerIssues(),
		Warnings:             status.WarningIssues(),
	}, nil
}
