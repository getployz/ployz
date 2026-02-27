package overlay

import (
	"context"
	"errors"
	"os"
)

func (c *Controller) Status(ctx context.Context, in Config) (Status, error) {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return Status{}, err
	}

	out := Status{StatePath: c.state.StatePath(cfg.DataDir)}
	s, err := c.state.Load(cfg.DataDir)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return out, nil
		}
		return Status{}, err
	}
	out.Configured = true
	out.Running = s.Phase == NetworkRunning
	out.Phase = s.Phase.String()
	expectedCorrosionMembers := c.expectedCorrosionMembers(ctx, cfg, s)

	wg, dockerNet, corr, probeErr := c.statusProber.ProbeInfra(ctx, s, expectedCorrosionMembers)
	if probeErr != nil {
		return Status{}, probeErr
	}
	out.WireGuard = wg
	out.DockerNet = dockerNet
	out.Corrosion = corr

	return out, nil
}

func (c *Controller) expectedCorrosionMembers(ctx context.Context, cfg Config, state *State) int {
	const minExpectedMembers = 1

	expected := minExpectedMembers
	if state == nil {
		return expected
	}
	if n := len(state.Bootstrap) + 1; n > expected {
		expected = n
	}

	registry := c.newRegistry(cfg.CorrosionAPIAddr, state.CorrosionAPIToken)
	if registry == nil {
		return expected
	}
	rows, err := registry.ListMachineRows(ctx)
	if err != nil {
		return expected
	}
	if len(rows) > expected {
		expected = len(rows)
	}

	return expected
}
