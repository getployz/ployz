package network

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

	wg, dockerNet, corr, probeErr := c.statusProber.ProbeInfra(ctx, s)
	if probeErr != nil {
		return Status{}, probeErr
	}
	out.WireGuard = wg
	out.DockerNet = dockerNet
	out.Corrosion = corr

	return out, nil
}
