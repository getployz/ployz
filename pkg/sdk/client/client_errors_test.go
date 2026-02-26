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
	err := grpcErr(preconditionError(t, types.PreconditionDeployNoMachinesAvailable, "deploy failed"))
	if !errors.Is(err, ErrPrecondition) {
		t.Fatalf("expected ErrPrecondition, got %v", err)
	}
	if !errors.Is(err, ErrNoMachinesAvailable) {
		t.Fatalf("expected ErrNoMachinesAvailable, got %v", err)
	}
}

func TestGRPCErrMapsRuntimeNotReadyPrecondition(t *testing.T) {
	err := grpcErr(preconditionError(t, types.PreconditionRuntimeNotReadyForServices, "runtime not ready"))
	if !errors.Is(err, ErrPrecondition) {
		t.Fatalf("expected ErrPrecondition, got %v", err)
	}
	if !errors.Is(err, ErrRuntimeNotReadyForServices) {
		t.Fatalf("expected ErrRuntimeNotReadyForServices, got %v", err)
	}
}

func TestGRPCErrMapsNetworkNotConfiguredPrecondition(t *testing.T) {
	err := grpcErr(preconditionError(t, types.PreconditionNetworkNotConfigured, "network missing"))
	if !errors.Is(err, ErrPrecondition) {
		t.Fatalf("expected ErrPrecondition, got %v", err)
	}
	if !errors.Is(err, ErrNetworkNotConfigured) {
		t.Fatalf("expected ErrNetworkNotConfigured, got %v", err)
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

func preconditionError(t *testing.T, code types.PreconditionCode, message string) error {
	t.Helper()
	st := status.New(codes.FailedPrecondition, message)
	withDetails, err := st.WithDetails(&errdetails.PreconditionFailure{
		Violations: []*errdetails.PreconditionFailure_Violation{{
			Type:        string(code),
			Subject:     "test",
			Description: message,
		}},
	})
	if err != nil {
		t.Fatalf("add precondition details: %v", err)
	}
	return withDetails.Err()
}
