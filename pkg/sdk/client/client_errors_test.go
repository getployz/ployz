package client

import (
	"errors"
	"testing"

	"ployz/pkg/sdk/types"

	"google.golang.org/genproto/googleapis/rpc/errdetails"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

func TestGRPCErrMapsNoMachinesPrecondition(t *testing.T) {
	err := grpcErr(preconditionError(t, types.PreconditionDeployNoMachinesAvailable, "deploy failed", "run `ployz machine add <user@host>`"))
	if !errors.Is(err, ErrPrecondition) {
		t.Fatalf("expected ErrPrecondition, got %v", err)
	}
	if !errors.Is(err, ErrNoMachinesAvailable) {
		t.Fatalf("expected ErrNoMachinesAvailable, got %v", err)
	}
}

func TestGRPCErrMapsRuntimeNotReadyPrecondition(t *testing.T) {
	err := grpcErr(preconditionError(t, types.PreconditionRuntimeNotReadyForServices, "runtime not ready", "run `ployz status` or `ployz doctor`"))
	if !errors.Is(err, ErrPrecondition) {
		t.Fatalf("expected ErrPrecondition, got %v", err)
	}
	if !errors.Is(err, ErrRuntimeNotReadyForServices) {
		t.Fatalf("expected ErrRuntimeNotReadyForServices, got %v", err)
	}
}

func TestGRPCErrMapsNetworkNotConfiguredPrecondition(t *testing.T) {
	err := grpcErr(preconditionError(t, types.PreconditionNetworkNotConfigured, "network missing", "run `ployz network create <network> --force`"))
	if !errors.Is(err, ErrPrecondition) {
		t.Fatalf("expected ErrPrecondition, got %v", err)
	}
	if !errors.Is(err, ErrNetworkNotConfigured) {
		t.Fatalf("expected ErrNetworkNotConfigured, got %v", err)
	}
}

func TestGRPCErrMapsDestroyWorkloadsPrecondition(t *testing.T) {
	err := grpcErr(preconditionError(t, types.PreconditionNetworkDestroyHasWorkloads, "destroy blocked", "remove workloads first with `ployz service remove <name>`"))
	if !errors.Is(err, ErrPrecondition) {
		t.Fatalf("expected ErrPrecondition, got %v", err)
	}
	if !errors.Is(err, ErrNetworkDestroyHasWorkloads) {
		t.Fatalf("expected ErrNetworkDestroyHasWorkloads, got %v", err)
	}
}

func TestGRPCErrMapsDestroyMachinesPrecondition(t *testing.T) {
	err := grpcErr(preconditionError(t, types.PreconditionNetworkDestroyHasMachines, "destroy blocked", "remove attached machines first with `ployz machine remove <id>`"))
	if !errors.Is(err, ErrPrecondition) {
		t.Fatalf("expected ErrPrecondition, got %v", err)
	}
	if !errors.Is(err, ErrNetworkDestroyHasMachines) {
		t.Fatalf("expected ErrNetworkDestroyHasMachines, got %v", err)
	}
}

func TestPreconditionHintReturnsStructuredHint(t *testing.T) {
	const hint = "run `ployz machine add <user@host>`"
	err := grpcErr(preconditionError(t, types.PreconditionDeployNoMachinesAvailable, "deploy failed", hint))
	if got := PreconditionHint(err); got != hint {
		t.Fatalf("expected hint %q, got %q", hint, got)
	}
}

func TestPreconditionHintEmptyWhenNoMetadata(t *testing.T) {
	err := grpcErr(preconditionError(t, types.PreconditionDeployNoMachinesAvailable, "deploy failed", ""))
	if got := PreconditionHint(err); got != "" {
		t.Fatalf("expected empty hint, got %q", got)
	}
}

func TestGRPCErrPreconditionWithoutDetails(t *testing.T) {
	err := grpcErr(status.Error(codes.FailedPrecondition, "plain precondition"))
	if !errors.Is(err, ErrPrecondition) {
		t.Fatalf("expected ErrPrecondition, got %v", err)
	}
	if errors.Is(err, ErrNoMachinesAvailable) {
		t.Fatalf("did not expect ErrNoMachinesAvailable, got %v", err)
	}
}

func preconditionError(t *testing.T, code types.PreconditionCode, message, hint string) error {
	t.Helper()
	st := status.New(codes.FailedPrecondition, message)
	precondition := &errdetails.PreconditionFailure{
		Violations: []*errdetails.PreconditionFailure_Violation{{
			Type:        string(code),
			Subject:     "test",
			Description: message,
		}},
	}
	var (
		withDetails *status.Status
		err         error
	)
	if hint == "" {
		withDetails, err = st.WithDetails(precondition)
	} else {
		withDetails, err = st.WithDetails(precondition, &errdetails.ErrorInfo{
			Reason: "PRECONDITION_FAILED",
			Domain: "ployz.controlplane",
			Metadata: map[string]string{
				errorInfoMetadataPreconditionCode: string(code),
				errorInfoMetadataRemediationHint:  hint,
			},
		})
	}
	if err != nil {
		t.Fatalf("add precondition details: %v", err)
	}
	return withDetails.Err()
}
