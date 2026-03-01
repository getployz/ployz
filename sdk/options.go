package sdk

// DialOption configures how the SDK connects to a daemon.
type DialOption func(*dialConfig)

type dialConfig struct {
	sshPort          int
	remoteSocketPath string
}

// WithSSHPort sets the SSH port for remote connections.
func WithSSHPort(port int) DialOption {
	return func(c *dialConfig) { c.sshPort = port }
}

// WithRemoteSocketPath overrides the daemon socket path on the remote host.
func WithRemoteSocketPath(path string) DialOption {
	return func(c *dialConfig) { c.remoteSocketPath = path }
}
