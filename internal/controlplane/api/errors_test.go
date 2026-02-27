package api

import (
	"errors"
	"testing"

	"ployz/internal/controlplane/manager"
	"ployz/internal/deploy"
	"ployz/pkg/sdk/types"

	"google.golang.org/genproto/googleapis/rpc/errdetails"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

func TestToGRPCErrorNoMachinesPreconditionDetail(t *testing.T) {
	err := toGRPCError(deploy.ErrNoMachinesAvailable)
	st, ok := status.FromError(err)
	if !ok {
		t.Fatalf("expected grpc status error, got %T", err)
	}
	if st.Code() != codes.FailedPrecondition {
		t.Fatalf("expected FailedPrecondition, got %s", st.Code())
	}

	if !hasPreconditionViolation(st, string(types.PreconditionDeployNoMachinesAvailable)) {
		t.Fatalf("expected precondition detail %q, got %v", types.PreconditionDeployNoMachinesAvailable, st.Details())
	}
}

func TestToGRPCErrorRuntimePreconditionDetail(t *testing.T) {
	err := toGRPCError(manager.ErrRuntimeNotReadyForServices)
	st, ok := status.FromError(err)
	if !ok {
		t.Fatalf("expected grpc status error, got %T", err)
	}
	if st.Code() != codes.FailedPrecondition {
		t.Fatalf("expected FailedPrecondition, got %s", st.Code())
	}

	if !hasPreconditionViolation(st, string(types.PreconditionRuntimeNotReadyForServices)) {
		t.Fatalf("expected precondition detail %q, got %v", types.PreconditionRuntimeNotReadyForServices, st.Details())
	}
	if got := remediationHint(st, string(types.PreconditionRuntimeNotReadyForServices)); got == "" {
		t.Fatalf("expected remediation hint, got empty")
	}
}

func TestToGRPCErrorDestroyWorkloadsPreconditionDetail(t *testing.T) {
	err := toGRPCError(manager.ErrNetworkDestroyHasWorkloads)
	st, ok := status.FromError(err)
	if !ok {
		t.Fatalf("expected grpc status error, got %T", err)
	}
	if st.Code() != codes.FailedPrecondition {
		t.Fatalf("expected FailedPrecondition, got %s", st.Code())
	}

	if !hasPreconditionViolation(st, string(types.PreconditionNetworkDestroyHasWorkloads)) {
		t.Fatalf("expected precondition detail %q, got %v", types.PreconditionNetworkDestroyHasWorkloads, st.Details())
	}
	if got := remediationHint(st, string(types.PreconditionNetworkDestroyHasWorkloads)); got == "" {
		t.Fatalf("expected remediation hint, got empty")
	}
}

func TestToGRPCErrorDestroyMachinesPreconditionDetail(t *testing.T) {
	err := toGRPCError(manager.ErrNetworkDestroyHasMachines)
	st, ok := status.FromError(err)
	if !ok {
		t.Fatalf("expected grpc status error, got %T", err)
	}
	if st.Code() != codes.FailedPrecondition {
		t.Fatalf("expected FailedPrecondition, got %s", st.Code())
	}

	if !hasPreconditionViolation(st, string(types.PreconditionNetworkDestroyHasMachines)) {
		t.Fatalf("expected precondition detail %q, got %v", types.PreconditionNetworkDestroyHasMachines, st.Details())
	}
	if got := remediationHint(st, string(types.PreconditionNetworkDestroyHasMachines)); got == "" {
		t.Fatalf("expected remediation hint, got empty")
	}
}

func TestToGRPCErrorUnknownFallsBackToInternal(t *testing.T) {
	err := toGRPCError(errors.New("boom"))
	st, ok := status.FromError(err)
	if !ok {
		t.Fatalf("expected grpc status error, got %T", err)
	}
	if st.Code() != codes.Internal {
		t.Fatalf("expected Internal, got %s", st.Code())
	}
}

func hasPreconditionViolation(st *status.Status, code string) bool {
	if st == nil {
		return false
	}
	for _, detail := range st.Details() {
		failure, ok := detail.(*errdetails.PreconditionFailure)
		if !ok || failure == nil {
			continue
		}
		for _, violation := range failure.Violations {
			if violation != nil && violation.Type == code {
				return true
			}
		}
	}
	return false
}

func remediationHint(st *status.Status, code string) string {
	if st == nil {
		return ""
	}
	for _, detail := range st.Details() {
		errInfo, ok := detail.(*errdetails.ErrorInfo)
		if !ok || errInfo == nil {
			continue
		}
		if errInfo.Metadata[errorInfoMetadataPreconditionCode] != code {
			continue
		}
		return errInfo.Metadata[errorInfoMetadataRemediationHint]
	}
	return ""
}
