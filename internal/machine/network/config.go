package network

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
	defaultDarwinHelper = "ghcr.io/linuxserver/wireguard@sha256:2c33534e332d158e6cfebebef23acc044586b6f285ad422b92b03870db98cccd"
	defaultCorrosionImg = "ghcr.io/psviderski/corrosion@sha256:66f5ff30bf2d35d134973dab501380c6cf4c81134205fcf3b3528a605541aafd"
	defaultWireGuardMTU = 1280
)

var (
	defaultNetworkPrefix        = netip.MustParsePrefix("10.210.0.0/16")
	defaultCorrosionBootstrapIP = netip.MustParseAddr("127.0.0.1")
)

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
	CorrosionMemberID  uint64
	CorrosionAPIToken  string
	CorrosionBootstrap []string
	CorrosionGossipIP  netip.Addr
	CorrosionAPIAddr   netip.AddrPort
	CorrosionGossipAP  netip.AddrPort
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
		cfg.DataRoot = defaults.DataRoot()
	}
	cfg.DataDir = filepath.Join(cfg.DataRoot, cfg.Network)
	if cfg.WGPort == 0 {
		cfg.WGPort = defaults.WGPort(cfg.Network)
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
	cfg.CorrosionAPIToken = strings.TrimSpace(cfg.CorrosionAPIToken)

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
