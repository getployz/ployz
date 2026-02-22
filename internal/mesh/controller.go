package mesh

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

// WithClock sets the clock used for timestamps.
func WithClock(clock Clock) Option {
	return func(c *Controller) { c.clock = clock }
}

// WithContainerRuntime sets the container runtime backend.
func WithContainerRuntime(rt ContainerRuntime) Option {
	return func(c *Controller) { c.containers = rt }
}

// WithCorrosionRuntime sets the corrosion container lifecycle backend.
func WithCorrosionRuntime(rt CorrosionRuntime) Option {
	return func(c *Controller) { c.corrosion = rt }
}

// WithStatusProber sets the infrastructure status prober.
func WithStatusProber(p StatusProber) Option {
	return func(c *Controller) { c.statusProber = p }
}

// WithPlatformOps sets the platform-specific runtime operations.
func WithPlatformOps(ops PlatformOps) Option {
	return func(c *Controller) { c.platformOps = ops }
}

// New creates a Controller with the given options.
func New(opts ...Option) (*Controller, error) {
	c := &Controller{}
	for _, o := range opts {
		o(c)
	}
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
