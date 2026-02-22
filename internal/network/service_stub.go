//go:build !linux && !darwin

package network

import (
	"context"
	"errors"
)

func New(opts ...Option) (*Controller, error) {
	c := &Controller{state: sqliteStateStore{}}
	for _, o := range opts {
		o(c)
	}
	return c, nil
}

func (c *Controller) Start(_ context.Context, _ Config) (Config, error) {
	return Config{}, errors.New("controller start is supported on linux only")
}

func (c *Controller) Stop(_ context.Context, _ Config, _ bool) (Config, error) {
	return Config{}, errors.New("controller stop is supported on linux only")
}

func (c *Controller) Status(_ context.Context, in Config) (Status, error) {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return Status{}, err
	}
	return Status{StatePath: statePath(cfg.DataDir)}, nil
}

func (c *Controller) applyPeerConfig(_ context.Context, _ Config, _ *State, _ []Peer) error {
	return errors.New("peer configuration is supported on linux and darwin only")
}

func (c *Controller) Reconcile(_ context.Context, _ Config) (int, error) {
	return 0, errors.New("reconcile is supported on linux and darwin only")
}

func (c *Controller) ReconcilePeers(_ context.Context, _ Config, _ []MachineRow) (int, error) {
	return 0, errors.New("peer reconcile is supported on linux and darwin only")
}

func (c *Controller) ListMachines(_ context.Context, _ Config) ([]Machine, error) {
	return nil, errors.New("machine listing is supported on linux and darwin only")
}

func (c *Controller) RemoveMachine(_ context.Context, _ Config, _ string) error {
	return errors.New("machine removal is supported on linux and darwin only")
}

func (c *Controller) UpsertMachine(_ context.Context, _ Config, _ Machine) error {
	return errors.New("machine upsert is supported on linux and darwin only")
}
