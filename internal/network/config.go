package network

import (
	"fmt"
	"net/netip"
	"path/filepath"
	"strings"

	"ployz/pkg/sdk/defaults"
)

const (
	defaultCorrosionImage = "ghcr.io/psviderski/corrosion@sha256:66f5ff30bf2d35d134973dab501380c6cf4c81134205fcf3b3528a605541aafd"
	// defaultWireGuardMTU is 1280: safe minimum that avoids fragmentation across all tunnel encapsulations.
	defaultWireGuardMTU    = 1280
	maxInterfaceNameLength = 15 // Linux kernel IFNAMSIZ limit
)

var (
	defaultNetworkPrefix        = netip.MustParsePrefix("10.210.0.0/16")
	defaultCorrosionBootstrapIP = netip.MustParseAddr("127.0.0.1")
)

type Config struct {
	Network           string
	DataRoot          string
	DataDir           string
	NetworkCIDR       netip.Prefix
	Subnet            netip.Prefix
	Management        netip.Addr
	AdvertiseEndpoint string
	WGInterface       string
	WGPort            int

	DockerNetwork  string
	CorrosionName  string
	CorrosionImage string
	CorrosionUser  string
	HelperImage    string
	HelperName     string

	CorrosionDir            string
	CorrosionConfig         string
	CorrosionSchema         string
	CorrosionAdminSock      string
	CorrosionAPIPort        int
	CorrosionGossipPort     int
	CorrosionMemberID       uint64
	CorrosionAPIToken       string
	CorrosionBootstrap      []string
	CorrosionGossipIP       netip.Addr
	CorrosionAPIAddr        netip.AddrPort
	CorrosionGossipAddrPort netip.AddrPort
}

func NormalizeConfig(cfg Config) (Config, error) {
	if strings.TrimSpace(cfg.Network) == "" {
		cfg.Network = "default"
	}
	cfg.AdvertiseEndpoint = strings.TrimSpace(cfg.AdvertiseEndpoint)
	if cfg.AdvertiseEndpoint != "" {
		if _, err := netip.ParseAddrPort(cfg.AdvertiseEndpoint); err != nil {
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
	if cfg.CorrosionImage == "" {
		cfg.CorrosionImage = defaultCorrosionImage
	}
	cfg.CorrosionBootstrap = normalizeBootstrapAddrs(cfg.CorrosionBootstrap)
	cfg.CorrosionAPIToken = strings.TrimSpace(cfg.CorrosionAPIToken)

	cfg.CorrosionDir = filepath.Join(cfg.DataDir, "corrosion")
	cfg.CorrosionConfig = filepath.Join(cfg.CorrosionDir, "config.toml")
	cfg.CorrosionSchema = filepath.Join(cfg.CorrosionDir, "schema.sql")
	cfg.CorrosionAdminSock = filepath.Join(cfg.CorrosionDir, "admin.sock")

	refreshCorrosionGossipAddr(&cfg)

	cfg.CorrosionAPIPort = defaults.CorrosionAPIPort(cfg.Network)
	cfg.CorrosionGossipPort = defaults.CorrosionGossipPort(cfg.Network)
	cfg.CorrosionAPIAddr = netip.AddrPortFrom(netip.MustParseAddr("127.0.0.1"), uint16(cfg.CorrosionAPIPort))
	cfg.CorrosionGossipAddrPort = netip.AddrPortFrom(cfg.CorrosionGossipIP, uint16(cfg.CorrosionGossipPort))
	return cfg, nil
}

func InterfaceName(network string) string {
	name := "plz-" + network
	if len(name) <= maxInterfaceNameLength {
		return name
	}
	return name[:maxInterfaceNameLength]
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
