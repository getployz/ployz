package defaults

import (
	"hash/fnv"
	"os"
	"path/filepath"
	"runtime"
	"strings"
)

const (
	defaultLinuxDataRoot  = "/var/lib/ployz/networks"
	defaultDarwinDataRoot = "Library/Application Support/ployz/networks"
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

func WGPort(network string) int {
	n := strings.TrimSpace(network)
	if n == "" || n == "default" {
		return 51820
	}
	return 51821 + int(hashMod(n, 500))
}

func HelperName(network string) string {
	n := strings.TrimSpace(network)
	if n == "" {
		n = "default"
	}
	return "ployz-helper-" + n
}

func CorrosionGossipPort(network string) int {
	return 53000 + int(networkOffset(network))
}

func CorrosionAPIPort(network string) int {
	return 52000 + int(networkOffset(network))
}

func NormalizeNetwork(network string) string {
	network = strings.TrimSpace(network)
	if network == "" {
		return "default"
	}
	return network
}

func networkOffset(network string) uint32 {
	n := strings.TrimSpace(network)
	if n == "" {
		n = "default"
	}
	return hashMod(n, 800)
}

func hashMod(s string, m uint32) uint32 {
	h := fnv.New32a()
	_, _ = h.Write([]byte(s))
	return h.Sum32() % m
}
