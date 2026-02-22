package defaults

import (
	"fmt"
	"hash/fnv"
	"os"
	"path/filepath"
	"runtime"
	"strings"
)

const (
	defaultLinuxDataRoot  = "/var/lib/ployz/networks"
	defaultDarwinDataRoot = "Library/Application Support/ployz/networks"

	// defaultWireGuardPort is 51820: the standard WireGuard port, used for the "default" network.
	defaultWireGuardPort = 51820
	// wireGuardPortRangeStart is 51821: non-default networks hash into 51821..52320.
	wireGuardPortRangeStart = 51821
	// wireGuardPortRangeSize is 500: covers enough networks to avoid collisions with reasonable probability.
	wireGuardPortRangeSize = 500

	// corrosionGossipPortBase is 53000: chosen to avoid conflicts with common services.
	corrosionGossipPortBase = 53000
	// corrosionAPIPortBase is 52000: local-only Corrosion HTTP API.
	corrosionAPIPortBase = 52000
	// daemonAPIPortBase is 54000: daemon gRPC API port.
	daemonAPIPortBase = 54000
	// networkOffsetRange is 800: FNV-1a hash modulus for port offset derivation.
	networkOffsetRange = 800
)

func DataRoot() string {
	if runtime.GOOS == "darwin" {
		home, err := os.UserHomeDir()
		if err != nil {
			return defaultLinuxDataRoot
		}
		return filepath.Join(home, defaultDarwinDataRoot)
	}
	return defaultLinuxDataRoot
}

// EnsureDataRoot creates the data root directory if it doesn't exist.
func EnsureDataRoot(dataRoot string) error {
	if dataRoot == "" {
		dataRoot = DataRoot()
	}
	if err := os.MkdirAll(dataRoot, 0o755); err != nil {
		return fmt.Errorf("create data root: %w", err)
	}
	return nil
}

func WGPort(network string) int {
	n := NormalizeNetwork(network)
	if n == "default" {
		return defaultWireGuardPort
	}
	return wireGuardPortRangeStart + int(hashMod(n, wireGuardPortRangeSize))
}

func HelperName(network string) string {
	return "ployz-helper-" + NormalizeNetwork(network)
}

func CorrosionGossipPort(network string) int {
	return corrosionGossipPortBase + int(networkOffset(network))
}

func CorrosionAPIPort(network string) int {
	return corrosionAPIPortBase + int(networkOffset(network))
}

func DaemonAPIPort(network string) int {
	return daemonAPIPortBase + int(networkOffset(network))
}

func NormalizeNetwork(network string) string {
	network = strings.TrimSpace(network)
	if network == "" {
		return "default"
	}
	return network
}

func networkOffset(network string) uint32 {
	return hashMod(NormalizeNetwork(network), networkOffsetRange)
}

func hashMod(s string, m uint32) uint32 {
	h := fnv.New32a()
	_, _ = h.Write([]byte(s))
	return h.Sum32() % m
}
