package machine

import (
	"context"
	"fmt"
	"log/slog"
	"strings"
	"time"

	"ployz/pkg/sdk/client"
	"ployz/pkg/sdk/telemetry"
	"ployz/pkg/sdk/types"

	"go.opentelemetry.io/otel/trace"
)

const (
	// teardownPerMachineTimeout bounds each remote DisableNetwork call.
	teardownPerMachineTimeout = 30 * time.Second
)

// TeardownOptions configures a cluster-wide network teardown.
type TeardownOptions struct {
	Network string
	Tracer  trace.Tracer
}

// TeardownResult summarizes a cluster teardown operation.
type TeardownResult struct {
	Total     int
	Succeeded int
	Failed    []TeardownError
}

// TeardownError records a per-machine teardown failure.
type TeardownError struct {
	MachineID string
	Error     error
}

// TeardownCluster disables and purges a network across all machines in the
// cluster. It uses the gRPC proxy to reach remote machines over the WireGuard
// overlay, tearing down remotes first (while the overlay is still up) and the
// local machine last.
func (s *Service) TeardownCluster(ctx context.Context, opts TeardownOptions) (TeardownResult, error) {
	log := slog.With("component", "machine-teardown")

	tracer := opts.Tracer
	if tracer == nil {
		tracer = s.tracer
	}

	op, err := telemetry.EmitPlan(ctx, tracer, "machine.teardown", telemetry.Plan{Steps: []telemetry.PlannedStep{
		{ID: "discover", Title: "discovering cluster machines"},
		{ID: "remote", Title: "tearing down remote machines"},
		{ID: "local", Title: "tearing down local"},
	}})
	if err != nil {
		return TeardownResult{}, err
	}

	var endErr error
	defer func() {
		op.End(endErr)
	}()

	var (
		machines []types.MachineEntry
		localID  string
	)

	discoverErr := op.RunStep(op.Context(), "discover", func(stepCtx context.Context) error {
		listed, listErr := s.api.ListMachines(stepCtx)
		if listErr != nil {
			return fmt.Errorf("list machines: %w", listErr)
		}
		identity, identityErr := s.api.GetIdentity(stepCtx)
		if identityErr != nil {
			return fmt.Errorf("get identity: %w", identityErr)
		}

		machines = listed
		localID = strings.TrimSpace(identity.ID)
		if localID == "" {
			localID = "local"
		}
		return nil
	})
	if discoverErr != nil {
		log.Warn("failed discovery, falling back to local-only teardown", "err", discoverErr)

		result := TeardownResult{Total: 1}
		localErr := op.RunStep(op.Context(), "local", func(stepCtx context.Context) error {
			return s.api.DisableNetwork(stepCtx, true)
		})
		if localErr != nil {
			result.Failed = []TeardownError{{MachineID: "local", Error: localErr}}
			endErr = localErr
		} else {
			result.Succeeded = 1
			endErr = discoverErr
		}
		return result, nil
	}

	remoteIDs := make([]string, 0, len(machines))
	for _, m := range machines {
		machineID := strings.TrimSpace(m.ID)
		if machineID == "" || machineID == localID {
			continue
		}
		remoteIDs = append(remoteIDs, machineID)
	}

	result := TeardownResult{Total: len(remoteIDs) + 1}

	remoteErr := op.RunStep(op.Context(), "remote", func(remoteCtx context.Context) error {
		failedRemote := 0
		for _, machineID := range remoteIDs {
			stepID := "remote/" + machineID
			machineErr := op.RunStep(remoteCtx, stepID, func(stepCtx context.Context) error {
				return teardownRemote(stepCtx, s.api, machineID)
			})
			if machineErr != nil {
				failedRemote++
				log.Warn("remote teardown failed", "machine", machineID, "err", machineErr)
				result.Failed = append(result.Failed, TeardownError{MachineID: machineID, Error: machineErr})
				continue
			}
			log.Info("remote teardown succeeded", "machine", machineID)
			result.Succeeded++
		}
		if failedRemote > 0 {
			return fmt.Errorf("%d/%d remote teardown failed", failedRemote, len(remoteIDs))
		}
		return nil
	})
	if remoteErr != nil {
		endErr = remoteErr
	}

	localErr := op.RunStep(op.Context(), "local", func(stepCtx context.Context) error {
		return s.api.DisableNetwork(stepCtx, true)
	})
	if localErr != nil {
		log.Warn("local teardown failed", "err", localErr)
		result.Failed = append(result.Failed, TeardownError{MachineID: localID, Error: localErr})
	} else {
		log.Info("local teardown succeeded")
		result.Succeeded++
	}

	if len(result.Failed) > 0 {
		endErr = fmt.Errorf("teardown completed with %d failed machine(s)", len(result.Failed))
	}
	return result, nil
}

func teardownRemote(ctx context.Context, api client.API, machineID string) error {
	machineCtx, cancel := context.WithTimeout(ctx, teardownPerMachineTimeout)
	defer cancel()

	proxyCtx := client.ProxyMachinesContext(machineCtx, []string{machineID})
	return api.DisableNetwork(proxyCtx, true)
}
