package machine

import (
	"context"
	"net"
	"net/netip"

	"ployz/internal/machine/corroservice"

	"github.com/docker/docker/client"
)

func configureCorrosion(cfg Config) error {
	return corroservice.WriteConfig(corroservice.Config{
		Dir:          cfg.CorrosionDir,
		SchemaPath:   cfg.CorrosionSchema,
		ConfigPath:   cfg.CorrosionConfig,
		AdminSock:    cfg.CorrosionAdminSock,
		Bootstrap:    cfg.CorrosionBootstrap,
		GossipAddr:   cfg.CorrosionGossipAP,
		APIAddr:      cfg.CorrosionAPIAddr,
		GossipMaxMTU: corrosionGossipMaxMTU(cfg.CorrosionGossipIP),
		User:         cfg.CorrosionUser,
	})
}

func startCorrosion(ctx context.Context, cli *client.Client, cfg Config) error {
	return corroservice.Start(ctx, cli, corroservice.RuntimeConfig{
		Name:       cfg.CorrosionName,
		Image:      cfg.CorrosionImg,
		ConfigPath: cfg.CorrosionConfig,
		DataDir:    cfg.CorrosionDir,
		User:       cfg.CorrosionUser,
		APIAddr:    cfg.CorrosionAPIAddr,
	})
}

func stopCorrosion(ctx context.Context, cli *client.Client, name string) error {
	return corroservice.Stop(ctx, cli, name)
}

func corrosionGossipMaxMTU(addr netip.Addr) int {
	const udpHeaderLen = 8
	if addr.Is4() {
		return defaultWireGuardMTU - net.IPv4len - udpHeaderLen
	}
	return defaultWireGuardMTU - net.IPv6len - udpHeaderLen
}
