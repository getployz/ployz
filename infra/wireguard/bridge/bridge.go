// Package bridge implements an in-process userspace WireGuard tunnel
// backed by gVisor's netstack. It provides a net.Conn-compatible
// DialContext for reaching overlay IPs without a TUN device or kernel
// involvement. Used on macOS to bridge the daemon process into the
// containerized WireGuard overlay.
//
// Future: swap netstack for a real TUN device to give the entire Mac
// overlay access, not just the daemon process.
package bridge

import (
	"context"
	"fmt"
	"net"
	"net/netip"
	"strings"

	"golang.zx2c4.com/wireguard/conn"
	"golang.zx2c4.com/wireguard/device"
	"golang.zx2c4.com/wireguard/tun/netstack"
	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

// Config holds the configuration for a netstack WireGuard bridge.
type Config struct {
	// PrivateKey is the bridge's ephemeral private key.
	PrivateKey wgtypes.Key
	// LocalIP is the bridge's overlay address (derived from its public key).
	LocalIP netip.Addr
	// PeerPublicKey is the container WG's public key (the node key).
	PeerPublicKey wgtypes.Key
	// PeerEndpoint is the container WG's host-reachable address (127.0.0.1:51820).
	PeerEndpoint netip.AddrPort
	// AllowedPrefix is the overlay prefix routed through this tunnel.
	AllowedPrefix netip.Prefix
	// MTU for the virtual interface.
	MTU int
}

// Bridge is an in-process userspace WireGuard tunnel. It provides
// overlay network connectivity to the daemon process without requiring
// a TUN device or kernel WireGuard module.
type Bridge struct {
	dev  *device.Device
	tnet *netstack.Net
}

// New creates and starts a netstack WireGuard bridge.
func New(cfg Config) (*Bridge, error) {
	tunDev, tnet, err := netstack.CreateNetTUN(
		[]netip.Addr{cfg.LocalIP},
		nil, // no DNS
		cfg.MTU,
	)
	if err != nil {
		return nil, fmt.Errorf("create netstack tun: %w", err)
	}

	dev := device.NewDevice(tunDev, conn.NewDefaultBind(), device.NewLogger(device.LogLevelSilent, ""))

	if err := dev.IpcSet(buildIPC(cfg)); err != nil {
		dev.Close()
		return nil, fmt.Errorf("configure bridge device: %w", err)
	}
	if err := dev.Up(); err != nil {
		dev.Close()
		return nil, fmt.Errorf("bring up bridge device: %w", err)
	}

	return &Bridge{dev: dev, tnet: tnet}, nil
}

// DialContext dials an address through the WireGuard tunnel.
func (b *Bridge) DialContext(ctx context.Context, network, addr string) (net.Conn, error) {
	return b.tnet.DialContext(ctx, network, addr)
}

// Close tears down the bridge device.
func (b *Bridge) Close() error {
	if b.dev != nil {
		b.dev.Close()
	}
	return nil
}

// buildIPC creates the WireGuard UAPI config string.
func buildIPC(cfg Config) string {
	var b strings.Builder
	fmt.Fprintf(&b, "private_key=%x\n", cfg.PrivateKey[:])
	fmt.Fprintf(&b, "listen_port=0\n")
	fmt.Fprintf(&b, "replace_peers=true\n")
	fmt.Fprintf(&b, "public_key=%x\n", cfg.PeerPublicKey[:])
	fmt.Fprintf(&b, "endpoint=%s\n", cfg.PeerEndpoint)
	fmt.Fprintf(&b, "allowed_ip=%s\n", cfg.AllowedPrefix)
	fmt.Fprintf(&b, "persistent_keepalive_interval=25\n")
	return b.String()
}
