package fake

import (
	"context"
	"fmt"
	"sync"

	"ployz/internal/adapter/fake/fault"
	"ployz/internal/check"
	"ployz/internal/deploy"
)

var _ deploy.HealthChecker = (*HealthChecker)(nil)

const FaultHealthCheckerWaitHealthy = "health_checker.wait_healthy"

// HealthChecker is an in-memory implementation of deploy.HealthChecker.
type HealthChecker struct {
	CallRecorder
	mu      sync.Mutex
	results map[string]error
	faults  *fault.Injector

	WaitHealthyErr func(ctx context.Context, containerName string) error
}

func NewHealthChecker() *HealthChecker {
	return &HealthChecker{results: make(map[string]error), faults: fault.NewInjector()}
}

func (h *HealthChecker) FailOnce(point string, err error) {
	h.faults.FailOnce(point, err)
}

func (h *HealthChecker) FailAlways(point string, err error) {
	h.faults.FailAlways(point, err)
}

func (h *HealthChecker) SetFaultHook(point string, hook fault.Hook) {
	h.faults.SetHook(point, hook)
}

func (h *HealthChecker) ClearFault(point string) {
	h.faults.Clear(point)
}

func (h *HealthChecker) ResetFaults() {
	h.faults.Reset()
}

func (h *HealthChecker) evalFault(point string, args ...any) error {
	check.Assert(h != nil, "HealthChecker.evalFault: receiver must not be nil")
	check.Assert(h.faults != nil, "HealthChecker.evalFault: faults injector must not be nil")
	if h == nil || h.faults == nil {
		return nil
	}
	return h.faults.Eval(point, args...)
}

// SetHealthy configures containerName as healthy.
func (h *HealthChecker) SetHealthy(containerName string) {
	h.mu.Lock()
	h.results[containerName] = nil
	h.mu.Unlock()
}

// SetUnhealthy configures containerName as unhealthy.
func (h *HealthChecker) SetUnhealthy(containerName string, err error) {
	h.mu.Lock()
	h.results[containerName] = err
	h.mu.Unlock()
}

func (h *HealthChecker) WaitHealthy(ctx context.Context, containerName string, cfg deploy.HealthCheck) error {
	h.record("WaitHealthy", containerName, cfg)
	if err := h.evalFault(FaultHealthCheckerWaitHealthy, ctx, containerName, cfg); err != nil {
		return err
	}
	if h.WaitHealthyErr != nil {
		if err := h.WaitHealthyErr(ctx, containerName); err != nil {
			return err
		}
	}

	h.mu.Lock()
	result, ok := h.results[containerName]
	h.mu.Unlock()
	if !ok {
		return fmt.Errorf("container %q: no health result configured", containerName)
	}
	return result
}
