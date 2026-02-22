package mesh

import "ployz/internal/check"

// Option configures a Controller.
type Option func(*Controller)

// WithRegistryFactory sets the factory used to create Registry instances.
func WithRegistryFactory(f RegistryFactory) Option {
	check.Assert(f != nil, "WithRegistryFactory: factory must not be nil")
	return func(c *Controller) { c.newRegistry = f }
}

// WithStateStore sets the state persistence backend.
func WithStateStore(s StateStore) Option {
	check.Assert(s != nil, "WithStateStore: store must not be nil")
	return func(c *Controller) { c.state = s }
}

// WithClock sets the clock used for timestamps.
func WithClock(clock Clock) Option {
	check.Assert(clock != nil, "WithClock: clock must not be nil")
	return func(c *Controller) { c.clock = clock }
}

// WithContainerRuntime sets the container runtime backend.
func WithContainerRuntime(rt ContainerRuntime) Option {
	check.Assert(rt != nil, "WithContainerRuntime: runtime must not be nil")
	return func(c *Controller) { c.containers = rt }
}

// WithCorrosionRuntime sets the corrosion container lifecycle backend.
func WithCorrosionRuntime(rt CorrosionRuntime) Option {
	check.Assert(rt != nil, "WithCorrosionRuntime: runtime must not be nil")
	return func(c *Controller) { c.corrosion = rt }
}

// WithStatusProber sets the infrastructure status prober.
func WithStatusProber(p StatusProber) Option {
	check.Assert(p != nil, "WithStatusProber: prober must not be nil")
	return func(c *Controller) { c.statusProber = p }
}

// WithPlatformOps sets the platform-specific runtime operations.
func WithPlatformOps(ops PlatformOps) Option {
	check.Assert(ops != nil, "WithPlatformOps: ops must not be nil")
	return func(c *Controller) { c.platformOps = ops }
}

// New creates a Controller with the given options.
func New(opts ...Option) (*Controller, error) {
	c := &Controller{}
	for _, o := range opts {
		o(c)
	}
	check.Assert(c.state != nil, "Controller.New: StateStore is required")
	check.Assert(c.clock != nil, "Controller.New: Clock is required")
	check.Assert(c.newRegistry != nil, "Controller.New: RegistryFactory is required")
	check.Assert(c.platformOps != nil, "Controller.New: PlatformOps is required")
	check.Assert(c.containers != nil, "Controller.New: ContainerRuntime is required")
	check.Assert(c.corrosion != nil, "Controller.New: CorrosionRuntime is required")
	check.Assert(c.statusProber != nil, "Controller.New: StatusProber is required")
	return c, nil
}

type Controller struct {
	containers   ContainerRuntime
	corrosion    CorrosionRuntime
	statusProber StatusProber
	platformOps  PlatformOps
	newRegistry  RegistryFactory
	state        StateStore
	clock        Clock
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
	if c == nil || c.containers == nil {
		return nil
	}
	return c.containers.Close()
}
