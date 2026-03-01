//go:build darwin

package platform

import (
	"context"
	"fmt"
	"log/slog"
	"net"
	"net/netip"
	"time"

	"ployz"
	"ployz/infra/wireguard"
	"ployz/infra/wireguard/bridge"
	wgcontainer "ployz/infra/wireguard/container"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

// BridgedWG wraps a container WireGuard with an in-process netstack
// bridge. The bridge gives the daemon process a dialer that can reach
// overlay IPs inside the container's network namespace.
//
// Satisfies mesh.WireGuard. The bridge is started during Up() after
// the container WG is running.
type BridgedWG struct {
	container *wgcontainer.WG
	bridgeCfg bridge.Config
	bridge    *bridge.Bridge
}

// NewBridgedWG creates a BridgedWG. The bridge is configured but not
// started until Up() is called (container must be running first).
func NewBridgedWG(containerWG *wgcontainer.WG, nodeKey wgtypes.Key) *BridgedWG {
	bridgePriv, _ := wgtypes.GeneratePrivateKey()
	bridgePub := bridgePriv.PublicKey()
	bridgeIP := ployz.ManagementIPFromKey(bridgePub)
	mgmtPrefix := netip.MustParsePrefix(ployz.ManagementCIDR)

	return &BridgedWG{
		container: containerWG,
		bridgeCfg: bridge.Config{
			PrivateKey:    bridgePriv,
			LocalIP:       bridgeIP,
			PeerPublicKey: nodeKey.PublicKey(),
			PeerEndpoint:  netip.AddrPortFrom(netip.MustParseAddr("127.0.0.1"), WireGuardPort),
			AllowedPrefix: mgmtPrefix,
			MTU:           WireGuardMTU,
		},
	}
}

// Up starts the container WG, registers the bridge as a peer, then
// starts the netstack bridge. Idempotent — closes any stale bridge
// before creating a new one.
func (b *BridgedWG) Up(ctx context.Context) error {
	if err := b.container.Up(ctx); err != nil {
		return fmt.Errorf("container wireguard up: %w", err)
	}

	// Register bridge as a peer on the container WG. The bridge has
	// no fixed endpoint — the container learns it from the UDP handshake.
	bridgePub := b.bridgeCfg.PrivateKey.PublicKey()
	if err := b.container.AddPeer(ctx, wireguard.PeerOwnerBridge, bridgePub, b.bridgeCfg.LocalIP); err != nil {
		return fmt.Errorf("register bridge peer: %w", err)
	}

	// Close any stale bridge from a previous partial Up.
	if b.bridge != nil {
		b.bridge.Close()
		b.bridge = nil
	}

	br, err := bridge.New(b.bridgeCfg)
	if err != nil {
		return fmt.Errorf("start overlay bridge: %w", err)
	}
	b.bridge = br

	slog.Info("Overlay bridge started.", "localIP", b.bridgeCfg.LocalIP)
	return nil
}

// SetPeers delegates to the container WG. Bridge peers are protected
// from removal by the peer ownership system.
func (b *BridgedWG) SetPeers(ctx context.Context, peers []ployz.MachineRecord) error {
	return b.container.SetPeers(ctx, peers)
}

// Down tears down the bridge and the container WG.
func (b *BridgedWG) Down(ctx context.Context) error {
	if b.bridge != nil {
		b.bridge.Close()
		b.bridge = nil
	}
	return b.container.Down(ctx)
}

// DialContext dials through the bridge's WireGuard tunnel.
// Satisfies mesh.OverlayNet.
func (b *BridgedWG) DialContext(ctx context.Context, network, addr string) (net.Conn, error) {
	if b.bridge == nil {
		return nil, fmt.Errorf("overlay bridge not started")
	}
	return b.bridge.DialContext(ctx, network, addr)
}

// PeerHandshakes delegates to the container WG.
func (b *BridgedWG) PeerHandshakes(ctx context.Context) (map[wgtypes.Key]time.Time, error) {
	return b.container.PeerHandshakes(ctx)
}

// Verify BridgedWG satisfies the mesh.WireGuard interface at compile time.
var _ interface {
	Up(context.Context) error
	SetPeers(context.Context, []ployz.MachineRecord) error
	Down(context.Context) error
} = (*BridgedWG)(nil)
