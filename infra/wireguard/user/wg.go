//go:build darwin

package user

import (
	"context"
	"fmt"
	"log/slog"
	"net/netip"
	"strings"
	"sync"

	"ployz"
	"ployz/infra/wireguard"

	"golang.zx2c4.com/wireguard/conn"
	"golang.zx2c4.com/wireguard/device"
	"golang.zx2c4.com/wireguard/tun"
	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

const peerKeepaliveSec = 25

// Config holds the static configuration for a userspace WireGuard device.
type Config struct {
	Interface  string
	MTU        int
	PrivateKey wgtypes.Key
	Port       int
	MachineIP  netip.Addr
	MgmtIP     netip.Addr
	MgmtCIDR   string // e.g. "fd8c:88ad:7f06::/48"
}

// TUNProvider creates a TUN device for the WireGuard interface.
// In production this clones a provisioned fd from the privileged helper.
// In tests this can return a fake.
type TUNProvider func() (tun.Device, string, error)

// PrivilegedRunner executes a privileged command (ifconfig, route).
// In production this delegates to the privileged helper socket.
// In tests this can be a no-op or recorder.
type PrivilegedRunner func(ctx context.Context, name string, args ...string) ([]byte, error)

// WG implements mesh.WireGuard using userspace wireguard-go on macOS.
type WG struct {
	cfg     Config
	tunProv TUNProvider
	privRun PrivilegedRunner

	mu     sync.Mutex
	dev    *device.Device
	tunDev tun.Device
	ifName string
}

// New creates a userspace WireGuard implementation.
func New(cfg Config, tunProv TUNProvider, privRun PrivilegedRunner) *WG {
	return &WG{
		cfg:     cfg,
		tunProv: tunProv,
		privRun: privRun,
	}
}

// Up creates the userspace WireGuard device, configures it, and brings it up.
// If the device is already up this is a no-op, making retries after partial
// mesh startup failures safe.
func (w *WG) Up(ctx context.Context) error {
	w.mu.Lock()
	defer w.mu.Unlock()

	if w.dev != nil {
		return nil
	}

	tunDev, tunName, err := w.tunProv()
	if err != nil {
		return fmt.Errorf("create tun device: %w", err)
	}

	log := slog.With("component", "wireguard-darwin", "iface", tunName)
	log.Debug("tun device ready", "mtu", w.cfg.MTU)

	dev := device.NewDevice(tunDev, conn.NewDefaultBind(), device.NewLogger(device.LogLevelSilent, ""))

	ipcConf := buildIPC(w.cfg.PrivateKey, w.cfg.Port, nil)
	if err := dev.IpcSet(ipcConf); err != nil {
		dev.Close()
		return fmt.Errorf("configure wireguard device: %w", err)
	}
	if err := dev.Up(); err != nil {
		dev.Close()
		return fmt.Errorf("bring up wireguard device: %w", err)
	}

	if err := w.configureInterface(ctx, tunName); err != nil {
		dev.Close()
		return err
	}

	w.dev = dev
	w.tunDev = tunDev
	w.ifName = tunName

	log.Debug("wireguard active")
	return nil
}

// SetPeers replaces the current peer set and syncs routes.
func (w *WG) SetPeers(ctx context.Context, peers []ployz.MachineRecord) error {
	w.mu.Lock()
	defer w.mu.Unlock()

	if w.dev == nil {
		return fmt.Errorf("wireguard not up")
	}

	ipcConf := buildIPC(w.cfg.PrivateKey, w.cfg.Port, peers)
	if err := w.dev.IpcSet(ipcConf); err != nil {
		return fmt.Errorf("update wireguard config: %w", err)
	}

	return w.syncRoutes(ctx, peers)
}

// Down tears down the WireGuard device and removes routes.
func (w *WG) Down(ctx context.Context) error {
	w.mu.Lock()
	defer w.mu.Unlock()

	if w.dev == nil {
		return nil
	}

	log := slog.With("component", "wireguard-darwin", "iface", w.ifName)
	log.Debug("tearing down")

	// Remove management route (best-effort).
	if w.cfg.MgmtCIDR != "" {
		_, _ = w.privRun(ctx, "route", "-n", "delete", "-inet6", w.cfg.MgmtCIDR, "-interface", w.ifName)
	}

	w.dev.Close()
	w.dev = nil
	w.tunDev = nil
	w.ifName = ""

	log.Debug("stopped")
	return nil
}

func (w *WG) configureInterface(ctx context.Context, iface string) error {
	ipStr := w.cfg.MachineIP.String()
	if out, err := w.privRun(ctx, "ifconfig", iface, "inet", ipStr, ipStr, "up"); err != nil {
		return fmt.Errorf("configure %s address %s: %w: %s", iface, ipStr, err, strings.TrimSpace(string(out)))
	}

	if w.cfg.MgmtIP.IsValid() && w.cfg.MgmtIP.Is6() {
		if out, err := w.privRun(ctx, "ifconfig", iface, "inet6", w.cfg.MgmtIP.String(), "prefixlen", "128"); err != nil {
			slog.Debug("assign IPv6 management address failed (non-fatal)", "addr", w.cfg.MgmtIP, "err", err, "output", strings.TrimSpace(string(out)))
		}
	}

	return nil
}

func (w *WG) syncRoutes(ctx context.Context, peers []ployz.MachineRecord) error {
	iface := w.ifName

	for _, p := range peers {
		if !p.OverlayIP.IsValid() {
			continue
		}
		prefix := wireguard.HostPrefix(p.OverlayIP)
		cidr := prefix.String()
		if prefix.Addr().Is4() {
			_, _ = w.privRun(ctx, "route", "-n", "delete", "-net", cidr, "-interface", iface)
			if out, err := w.privRun(ctx, "route", "-n", "add", "-net", cidr, "-interface", iface); err != nil {
				return fmt.Errorf("add route %s via %s: %w: %s", cidr, iface, err, strings.TrimSpace(string(out)))
			}
		} else {
			_, _ = w.privRun(ctx, "route", "-n", "delete", "-inet6", cidr, "-interface", iface)
			if out, err := w.privRun(ctx, "route", "-n", "add", "-inet6", cidr, "-interface", iface); err != nil {
				slog.Debug("IPv6 route failed (non-fatal)", "cidr", cidr, "err", err, "output", strings.TrimSpace(string(out)))
			}
		}
	}

	if w.cfg.MgmtCIDR != "" {
		_, _ = w.privRun(ctx, "route", "-n", "delete", "-inet6", w.cfg.MgmtCIDR, "-interface", iface)
		if out, err := w.privRun(ctx, "route", "-n", "add", "-inet6", w.cfg.MgmtCIDR, "-interface", iface); err != nil {
			slog.Debug("IPv6 management route failed (non-fatal)", "err", err, "output", strings.TrimSpace(string(out)))
		}
	}

	return nil
}

func buildIPC(priv wgtypes.Key, port int, peers []ployz.MachineRecord) string {
	var b strings.Builder
	fmt.Fprintf(&b, "private_key=%x\nlisten_port=%d\nreplace_peers=true\n", priv[:], port)
	for _, p := range peers {
		fmt.Fprintf(&b, "public_key=%x\n", p.PublicKey[:])
		if len(p.Endpoints) > 0 {
			fmt.Fprintf(&b, "endpoint=%s\n", p.Endpoints[0].String())
		}
		if p.OverlayIP.IsValid() {
			fmt.Fprintf(&b, "allowed_ip=%s\n", wireguard.HostPrefix(p.OverlayIP).String())
		}
		fmt.Fprintf(&b, "persistent_keepalive_interval=%d\n", peerKeepaliveSec)
	}
	return b.String()
}
