package server

import (
	"errors"
	"os"
	"strings"

	"ployz/internal/mesh"

	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

// --- Error mapping ---

func toGRPCError(err error) error {
	if err == nil {
		return nil
	}

	if errors.Is(err, os.ErrNotExist) || errors.Is(err, mesh.ErrNotInitialized) {
		return status.Error(codes.NotFound, err.Error())
	}
	if errors.Is(err, mesh.ErrConflict) {
		return status.Error(codes.FailedPrecondition, err.Error())
	}
	var valErr *mesh.ValidationError
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
