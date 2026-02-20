package machine

import (
	"fmt"
	"net/netip"
	"os"
	"path/filepath"
	"runtime"
	"strings"

	"ployz/pkg/sdk/defaults"
)

const (
	defaultDarwinHelper = "ghcr.io/linuxserver/wireguard:latest"
	defaultCorrosionImg = "ghcr.io/psviderski/corrosion:latest"
	defaultWireGuardMTU = 1280
)

var defaultCorrosionBootstrapIP = netip.MustParseAddr("127.0.0.1")

type Config struct {
	Network     string
	DataRoot    string
	DataDir     string
	NetworkCIDR netip.Prefix
	Subnet      netip.Prefix
	Management  netip.Addr
	AdvertiseEP string
	WGInterface string
	WGPort      int

	DockerNetwork string
	CorrosionName string
	CorrosionImg  string
	CorrosionUser string
	HelperImage   string
	HelperName    string

	CorrosionDir       string
	CorrosionConfig    string
	CorrosionSchema    string
	CorrosionAdminSock string
	CorrosionAPIPort   int
	CorrosionGossip    int
	CorrosionBootstrap []string
	CorrosionGossipIP  netip.Addr
	CorrosionAPIAddr   netip.AddrPort
	CorrosionGossipAP  netip.AddrPort
}

func DefaultDataRoot() string {
	return defaults.DataRoot()
}

func NormalizeConfig(cfg Config) (Config, error) {
	if strings.TrimSpace(cfg.Network) == "" {
		cfg.Network = "default"
	}
	cfg.AdvertiseEP = strings.TrimSpace(cfg.AdvertiseEP)
	if cfg.AdvertiseEP != "" {
		if _, err := netip.ParseAddrPort(cfg.AdvertiseEP); err != nil {
			return cfg, fmt.Errorf("parse advertise endpoint: %w", err)
		}
	}
	if cfg.DataRoot == "" {
		cfg.DataRoot = DefaultDataRoot()
	}
	cfg.DataDir = filepath.Join(cfg.DataRoot, cfg.Network)
	if cfg.WGPort == 0 {
		cfg.WGPort = DefaultWGPort(cfg.Network)
	}
	if cfg.WGInterface == "" {
		cfg.WGInterface = InterfaceName(cfg.Network)
	}
	if cfg.DockerNetwork == "" {
		cfg.DockerNetwork = "ployz-" + cfg.Network
	}
	if cfg.CorrosionName == "" {
		cfg.CorrosionName = "ployz-corrosion-" + cfg.Network
	}
	if cfg.HelperName == "" {
		cfg.HelperName = "ployz-helper-" + cfg.Network
	}
	if cfg.CorrosionImg == "" {
		cfg.CorrosionImg = defaultCorrosionImg
	}
	if runtime.GOOS == "darwin" {
		if cfg.HelperImage == "" {
			cfg.HelperImage = os.Getenv("PLOYZ_ORB_HELPER_IMAGE")
		}
		if cfg.HelperImage == "" {
			cfg.HelperImage = defaultDarwinHelper
		}
	}
	for i := range cfg.CorrosionBootstrap {
		cfg.CorrosionBootstrap[i] = strings.TrimSpace(cfg.CorrosionBootstrap[i])
	}

	cfg.CorrosionDir = filepath.Join(cfg.DataDir, "corrosion")
	cfg.CorrosionConfig = filepath.Join(cfg.CorrosionDir, "config.toml")
	cfg.CorrosionSchema = filepath.Join(cfg.CorrosionDir, "schema.sql")
	cfg.CorrosionAdminSock = filepath.Join(cfg.CorrosionDir, "admin.sock")

	refreshCorrosionGossipAddr(&cfg)

	cfg.CorrosionAPIPort = defaults.CorrosionAPIPort(cfg.Network)
	cfg.CorrosionGossip = defaults.CorrosionGossipPort(cfg.Network)
	cfg.CorrosionAPIAddr = netip.AddrPortFrom(netip.MustParseAddr("127.0.0.1"), uint16(cfg.CorrosionAPIPort))
	cfg.CorrosionGossipAP = netip.AddrPortFrom(cfg.CorrosionGossipIP, uint16(cfg.CorrosionGossip))
	return cfg, nil
}

func InterfaceName(network string) string {
	name := "plz-" + network
	if len(name) <= 15 {
		return name
	}
	return name[:15]
}

func DefaultWGPort(network string) int {
	return defaults.WGPort(network)
}

func refreshCorrosionGossipAddr(cfg *Config) {
	if cfg.Management.IsValid() {
		cfg.CorrosionGossipIP = cfg.Management
		return
	}
	if !cfg.CorrosionGossipIP.IsValid() {
		cfg.CorrosionGossipIP = defaultCorrosionBootstrapIP
	}
}

func machineIP(subnet netip.Prefix) netip.Addr {
	return subnet.Masked().Addr().Next()
}
