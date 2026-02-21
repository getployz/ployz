package network

import "github.com/docker/docker/client"

type Controller struct {
	cli *client.Client
}

type Status struct {
	Configured bool
	Running    bool
	WireGuard  bool
	Corrosion  bool
	DockerNet  bool
	StatePath  string
}

func (c *Controller) Close() error {
	if c == nil || c.cli == nil {
		return nil
	}
	return c.cli.Close()
}
