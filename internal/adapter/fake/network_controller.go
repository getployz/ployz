package fake

import (
	"context"
	"sync"

	"ployz/internal/adapter/fake/fault"
	"ployz/internal/check"
	"ployz/internal/engine"
	"ployz/internal/mesh"
	"ployz/internal/reconcile"
)

var _ engine.NetworkController = (*NetworkController)(nil)

const FaultNetworkControllerStart = "network_controller.start"

// NetworkController tracks start/stop lifecycle for engine tests.
type NetworkController struct {
	CallRecorder
	mu           sync.Mutex
	Started      bool
	Closed       bool
	ReturnConfig mesh.Config
	faults       *fault.Injector

	StartErr func(ctx context.Context, cfg mesh.Config) error
}

// NewNetworkController creates a NetworkController that returns the given config on Start.
func NewNetworkController(returnCfg mesh.Config) *NetworkController {
	return &NetworkController{ReturnConfig: returnCfg, faults: fault.NewInjector()}
}

func (c *NetworkController) FailOnce(point string, err error) {
	c.faults.FailOnce(point, err)
}

func (c *NetworkController) FailAlways(point string, err error) {
	c.faults.FailAlways(point, err)
}

func (c *NetworkController) SetFaultHook(point string, hook fault.Hook) {
	c.faults.SetHook(point, hook)
}

func (c *NetworkController) ClearFault(point string) {
	c.faults.Clear(point)
}

func (c *NetworkController) ResetFaults() {
	c.faults.Reset()
}

func (c *NetworkController) evalFault(point string, args ...any) error {
	check.Assert(c != nil, "NetworkController.evalFault: receiver must not be nil")
	check.Assert(c.faults != nil, "NetworkController.evalFault: faults injector must not be nil")
	if c == nil || c.faults == nil {
		return nil
	}
	return c.faults.Eval(point, args...)
}

func (c *NetworkController) Start(ctx context.Context, cfg mesh.Config) (mesh.Config, error) {
	c.record("Start", cfg)
	if err := c.evalFault(FaultNetworkControllerStart, ctx, cfg); err != nil {
		return mesh.Config{}, err
	}
	if c.StartErr != nil {
		if err := c.StartErr(ctx, cfg); err != nil {
			return mesh.Config{}, err
		}
	}
	c.mu.Lock()
	defer c.mu.Unlock()

	c.Started = true
	return c.ReturnConfig, nil
}

func (c *NetworkController) Close() error {
	c.record("Close")
	c.mu.Lock()
	defer c.mu.Unlock()

	c.Closed = true
	return nil
}

// ControllerFactory returns an engine.NetworkControllerFactory that always returns ctrl.
func ControllerFactory(ctrl *NetworkController) engine.NetworkControllerFactory {
	return func() (engine.NetworkController, error) {
		return ctrl, nil
	}
}

// ReconcilerFactory returns an engine.PeerReconcilerFactory that always returns rec.
func ReconcilerFactory(rec *PeerReconciler) engine.PeerReconcilerFactory {
	return func() (reconcile.PeerReconciler, error) {
		return rec, nil
	}
}
