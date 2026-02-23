package fake

import (
	"context"
	"errors"
	"testing"

	"ployz/internal/deploy"
)

func TestHealthChecker_SetHealthy(t *testing.T) {
	ctx := t.Context()
	h := NewHealthChecker()
	h.SetHealthy("api-1")

	if err := h.WaitHealthy(ctx, "api-1", deploy.HealthCheck{}); err != nil {
		t.Fatalf("WaitHealthy() error = %v", err)
	}
}

func TestHealthChecker_SetUnhealthy(t *testing.T) {
	ctx := t.Context()
	h := NewHealthChecker()
	expected := errors.New("container unhealthy")
	h.SetUnhealthy("api-1", expected)

	err := h.WaitHealthy(ctx, "api-1", deploy.HealthCheck{})
	if !errors.Is(err, expected) {
		t.Fatalf("WaitHealthy() error = %v, want %v", err, expected)
	}
}

func TestHealthChecker_MissingResult(t *testing.T) {
	ctx := t.Context()
	h := NewHealthChecker()

	if err := h.WaitHealthy(ctx, "missing", deploy.HealthCheck{}); err == nil {
		t.Fatal("WaitHealthy() expected error for missing configured result")
	}
}

func TestHealthChecker_ErrorInjection(t *testing.T) {
	ctx := t.Context()
	h := NewHealthChecker()
	injected := errors.New("injected")
	h.WaitHealthyErr = func(ctx context.Context, containerName string) error { return injected }
	h.SetHealthy("api-1")

	err := h.WaitHealthy(ctx, "api-1", deploy.HealthCheck{})
	if !errors.Is(err, injected) {
		t.Fatalf("WaitHealthy() error = %v, want %v", err, injected)
	}
}

func TestHealthChecker_FaultFailOnce(t *testing.T) {
	ctx := t.Context()
	h := NewHealthChecker()
	injected := errors.New("injected")
	h.FailOnce(FaultHealthCheckerWaitHealthy, injected)
	h.SetHealthy("api-1")

	err := h.WaitHealthy(ctx, "api-1", deploy.HealthCheck{})
	if !errors.Is(err, injected) {
		t.Fatalf("first WaitHealthy() error = %v, want injected", err)
	}

	err = h.WaitHealthy(ctx, "api-1", deploy.HealthCheck{})
	if err != nil {
		t.Fatalf("second WaitHealthy() error = %v, want nil", err)
	}
}
