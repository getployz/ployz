package network

import "github.com/docker/docker/client"

// Option configures a Controller.
type Option func(*Controller)

// WithRegistryFactory sets the factory used to create Registry instances.
func WithRegistryFactory(f RegistryFactory) Option {
	return func(c *Controller) { c.newRegistry = f }
}

// WithStateStore sets the state persistence backend.
func WithStateStore(s StateStore) Option {
	return func(c *Controller) { c.state = s }
}

type Controller struct {
	cli         *client.Client
	newRegistry RegistryFactory
	state       StateStore
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
