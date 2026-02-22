package wireguard

const defaultTUNSocketPath = "/tmp/ployz-tun.sock"
const defaultPrivilegedSocketPath = "/tmp/ployz-priv.sock"

func DefaultTUNSocketPath() string {
	return defaultTUNSocketPath
}

func DefaultPrivilegedSocketPath() string {
	return defaultPrivilegedSocketPath
}
