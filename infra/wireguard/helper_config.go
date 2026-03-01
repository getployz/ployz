package wireguard

// HelperConfig configures the privileged helper runtime.
// On macOS, TUNSocketPath and MTU are used to provision the TUN fd to the daemon.
type HelperConfig struct {
	SocketPath    string
	Token         string
	TUNSocketPath string
	MTU           int
}
