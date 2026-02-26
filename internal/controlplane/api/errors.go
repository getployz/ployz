package api

import (
	"errors"
	"os"
	"strings"

	"ployz/internal/controlplane/manager"
	"ployz/internal/deploy"
	"ployz/internal/network"
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

	if errors.Is(err, os.ErrNotExist) || errors.Is(err, network.ErrNotInitialized) {
		return status.Error(codes.NotFound, err.Error())
	}
	if errors.Is(err, manager.ErrNetworkNotConfigured) {
		return preconditionStatus(types.PreconditionNetworkNotConfigured, "network", err.Error())
	}
	if errors.Is(err, manager.ErrRuntimeNotReadyForServices) {
		return preconditionStatus(types.PreconditionRuntimeNotReadyForServices, "runtime", err.Error())
	}
	if errors.Is(err, deploy.ErrNoMachinesAvailable) {
		return preconditionStatus(types.PreconditionDeployNoMachinesAvailable, "deploy", err.Error())
	}
	if errors.Is(err, network.ErrConflict) {
		return status.Error(codes.Aborted, err.Error())
	}
	var valErr *network.ValidationError
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

func preconditionStatus(code types.PreconditionCode, subject, message string) error {
	st := status.New(codes.FailedPrecondition, message)
	withDetails, err := st.WithDetails(&errdetails.PreconditionFailure{
		Violations: []*errdetails.PreconditionFailure_Violation{
			{
				Type:        string(code),
				Subject:     subject,
				Description: message,
			},
		},
	})
	if err != nil {
		return st.Err()
	}
	return withDetails.Err()
}
