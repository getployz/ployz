package service

import (
	"errors"
	"strings"
	"testing"

	"ployz/pkg/sdk/client"
)

func TestDecorateDeployPreconditionErrorNoMachines(t *testing.T) {
	err := errors.Join(client.ErrPrecondition, client.ErrNoMachinesAvailable)
	decorated := decorateDeployPreconditionError(err, "default")
	if decorated == nil {
		t.Fatal("expected error")
	}
	msg := decorated.Error()
	if !strings.Contains(msg, "no schedulable machines available for network \"default\"") {
		t.Fatalf("unexpected message: %q", msg)
	}
}

func TestDecorateDeployPreconditionErrorRuntimeNotReady(t *testing.T) {
	err := errors.Join(client.ErrPrecondition, client.ErrRuntimeNotReadyForServices)
	decorated := decorateDeployPreconditionError(err, "default")
	if decorated == nil {
		t.Fatal("expected error")
	}
	if !strings.Contains(decorated.Error(), "runtime is not ready for service deployment") {
		t.Fatalf("unexpected message: %q", decorated.Error())
	}
}

func TestDecorateDeployPreconditionErrorPassthrough(t *testing.T) {
	original := errors.New("boom")
	decorated := decorateDeployPreconditionError(original, "default")
	if decorated != original {
		t.Fatalf("expected passthrough error, got %v", decorated)
	}
}
