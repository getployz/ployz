package fake

import (
	"context"
	"sync"

	"ployz/internal/engine"
	"ployz/internal/network"
	"ployz/internal/reconcile"
)

var _ engine.NetworkController = (*NetworkController)(nil)

// NetworkController tracks start/stop lifecycle for engine tests.
type NetworkController struct {
	CallRecorder
	mu           sync.Mutex
	Started      bool
	Closed       bool
	ReturnConfig network.Config

	StartErr func(ctx context.Context, cfg network.Config) error
}

// NewNetworkController creates a NetworkController that returns the given config on Start.
func NewNetworkController(returnCfg network.Config) *NetworkController {
	return &NetworkController{ReturnConfig: returnCfg}
}

func (c *NetworkController) Start(ctx context.Context, cfg network.Config) (network.Config, error) {
	c.record("Start", cfg)
	if c.StartErr != nil {
		if err := c.StartErr(ctx, cfg); err != nil {
			return network.Config{}, err
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
