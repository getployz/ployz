package api

import (
	"errors"
	"os"
	"strings"

	"ployz/internal/daemon/manager"
	"ployz/internal/daemon/overlay"
	"ployz/pkg/sdk/types"

	"google.golang.org/genproto/googleapis/rpc/errdetails"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

// --- Error mapping ---

func toGRPCError(err error) error {
	if err == nil {
		return nil
	}

	if errors.Is(err, os.ErrNotExist) || errors.Is(err, overlay.ErrNotInitialized) {
		return status.Error(codes.NotFound, err.Error())
	}
	if errors.Is(err, manager.ErrNetworkNotConfigured) {
		return preconditionStatus(
			types.PreconditionNetworkNotConfigured,
			"network",
			err.Error(),
			"run `ployz network create <network> --force`",
		)
	}
	if errors.Is(err, manager.ErrRuntimeNotReadyForServices) {
		return preconditionStatus(
			types.PreconditionRuntimeNotReadyForServices,
			"runtime",
			err.Error(),
			"run `ployz status` or `ployz doctor`",
		)
	}
	if errors.Is(err, manager.ErrNetworkDestroyHasWorkloads) {
		return preconditionStatus(
			types.PreconditionNetworkDestroyHasWorkloads,
			"network",
			err.Error(),
			"remove workloads first with `ployz service remove <name>`",
		)
	}
	if errors.Is(err, manager.ErrNetworkDestroyHasMachines) {
		return preconditionStatus(
			types.PreconditionNetworkDestroyHasMachines,
			"network",
			err.Error(),
			"remove attached machines first with `ployz machine remove <id>`",
		)
	}
	var valErr *overlay.ValidationError
	if errors.As(err, &valErr) {
		return status.Error(codes.InvalidArgument, err.Error())
	}

	// Fallback to string matching for errors not yet converted to typed sentinels.
	msg := err.Error()

	if strings.Contains(msg, "is not initialized") {
		return status.Error(codes.NotFound, msg)
	}
	if strings.Contains(msg, "is required") ||
		strings.Contains(msg, "must be") ||
		strings.Contains(msg, "parse ") {
		return status.Error(codes.InvalidArgument, msg)
	}
	if strings.Contains(msg, "connect to docker") ||
		strings.Contains(msg, "docker daemon") {
		return status.Error(codes.Unavailable, msg)
	}

	return status.Error(codes.Internal, msg)
}

const (
	preconditionErrorInfoReason = "PRECONDITION_FAILED"
	preconditionErrorInfoDomain = "ployz.controlplane"

	errorInfoMetadataPreconditionCode = "precondition_code"
	errorInfoMetadataRemediationHint  = "remediation_hint"
)

func preconditionStatus(code types.PreconditionCode, subject, message, remediationHint string) error {
	st := status.New(codes.FailedPrecondition, message)
	withDetails, err := st.WithDetails(
		&errdetails.PreconditionFailure{
			Violations: []*errdetails.PreconditionFailure_Violation{
				{
					Type:        string(code),
					Subject:     subject,
					Description: message,
				},
			},
		},
		&errdetails.ErrorInfo{
			Reason: preconditionErrorInfoReason,
			Domain: preconditionErrorInfoDomain,
			Metadata: map[string]string{
				errorInfoMetadataPreconditionCode: string(code),
				errorInfoMetadataRemediationHint:  remediationHint,
			},
		},
	)
	if err != nil {
		return st.Err()
	}
	return withDetails.Err()
}
