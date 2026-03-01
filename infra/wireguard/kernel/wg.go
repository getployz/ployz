//go:build linux

package kernel

import (
	"context"
	"errors"
	"fmt"
	"net"
	"net/netip"
	"time"

	"ployz"

	"github.com/vishvananda/netlink"
	"golang.org/x/sys/unix"
	"golang.zx2c4.com/wireguard/wgctrl"
	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

const peerKeepalive = 25 * time.Second

// Config holds the static configuration for a kernel WireGuard device.
type Config struct {
	Interface  string
	MTU        int
	PrivateKey wgtypes.Key
	Port       int
	Addrs      []netip.Prefix
	MachineIP  netip.Addr
	MgmtIP     netip.Addr
}

// WG implements mesh.WireGuard using the Linux kernel WireGuard module.
type WG struct {
	cfg Config
}

// New creates a kernel WireGuard implementation.
func New(cfg Config) *WG {
	return &WG{cfg: cfg}
}

// Up creates the WireGuard interface, sets the private key and listen port,
// assigns addresses, and brings the link up.
func (w *WG) Up(_ context.Context) error {
	link, err := ensureLink(w.cfg.Interface, w.cfg.MTU)
	if err != nil {
		return err
	}

	wg, err := wgctrl.New()
	if err != nil {
		return fmt.Errorf("create wireguard client: %w", err)
	}
	defer wg.Close()

	wgCfg := wgtypes.Config{
		PrivateKey:   &w.cfg.PrivateKey,
		ListenPort:   &w.cfg.Port,
		ReplacePeers: true,
	}
	if err := wg.ConfigureDevice(w.cfg.Interface, wgCfg); err != nil {
		return fmt.Errorf("configure wireguard device: %w", err)
	}

	if err := syncAddresses(link, w.cfg.Addrs); err != nil {
		return err
	}

	if link.Attrs().Flags&unix.IFF_UP == 0 {
		if err := netlink.LinkSetUp(link); err != nil {
			return fmt.Errorf("set wireguard interface up: %w", err)
		}
	}

	return nil
}

// SetPeers replaces the current peer set with the desired state and syncs routes.
func (w *WG) SetPeers(_ context.Context, peers []ployz.MachineRecord) error {
	wg, err := wgctrl.New()
	if err != nil {
		return fmt.Errorf("create wireguard client: %w", err)
	}
	defer wg.Close()

	dev, err := wg.Device(w.cfg.Interface)
	if err != nil {
		return fmt.Errorf("inspect wireguard device: %w", err)
	}

	peerCfgs := buildPeerConfigs(dev, peers)

	wgCfg := wgtypes.Config{
		ReplacePeers: false,
		Peers:        peerCfgs,
	}
	if err := wg.ConfigureDevice(w.cfg.Interface, wgCfg); err != nil {
		return fmt.Errorf("configure wireguard peers: %w", err)
	}

	link, err := netlink.LinkByName(w.cfg.Interface)
	if err != nil {
		return fmt.Errorf("find wireguard interface %q: %w", w.cfg.Interface, err)
	}

	if err := syncRoutes(link, w.cfg.MachineIP, w.cfg.MgmtIP, peers); err != nil {
		return err
	}

	return nil
}

// Down removes the WireGuard interface.
func (w *WG) Down(_ context.Context) error {
	link, err := netlink.LinkByName(w.cfg.Interface)
	if err != nil {
		if _, ok := err.(netlink.LinkNotFoundError); ok {
			return nil
		}
		return fmt.Errorf("find wireguard interface %q: %w", w.cfg.Interface, err)
	}
	if err := netlink.LinkDel(link); err != nil {
		return fmt.Errorf("delete wireguard interface %q: %w", w.cfg.Interface, err)
	}
	return nil
}

func ensureLink(iface string, mtu int) (netlink.Link, error) {
	link, err := netlink.LinkByName(iface)
	if err != nil {
		if _, ok := err.(netlink.LinkNotFoundError); !ok {
			return nil, fmt.Errorf("find wireguard interface %q: %w", iface, err)
		}
		link = &netlink.GenericLink{LinkAttrs: netlink.LinkAttrs{Name: iface}, LinkType: "wireguard"}
		if err := netlink.LinkAdd(link); err != nil {
			return nil, fmt.Errorf("create wireguard interface %q: %w", iface, err)
		}
		link, err = netlink.LinkByName(iface)
		if err != nil {
			return nil, fmt.Errorf("refetch wireguard interface %q: %w", iface, err)
		}
	}
	if link.Attrs().MTU != mtu {
		if err := netlink.LinkSetMTU(link, mtu); err != nil {
			return nil, fmt.Errorf("set wireguard mtu on %q: %w", iface, err)
		}
	}
	return link, nil
}

// overlayPrefix returns a host prefix for the given overlay IP (/128 for v6, /32 for v4).
func overlayPrefix(ip netip.Addr) netip.Prefix {
	if ip.Is6() {
		return netip.PrefixFrom(ip, 128)
	}
	return netip.PrefixFrom(ip, 32)
}

func buildPeerConfigs(dev *wgtypes.Device, peers []ployz.MachineRecord) []wgtypes.PeerConfig {
	peerCfgs := make([]wgtypes.PeerConfig, 0, len(peers)+len(dev.Peers))
	desired := make(map[string]struct{}, len(peers))

	for _, p := range peers {
		var allowedIPs []net.IPNet
		if p.OverlayIP.IsValid() {
			allowedIPs = []net.IPNet{prefixToIPNet(overlayPrefix(p.OverlayIP))}
		}
		pc := wgtypes.PeerConfig{
			PublicKey:                   p.PublicKey,
			ReplaceAllowedIPs:           true,
			AllowedIPs:                  allowedIPs,
			PersistentKeepaliveInterval: ptrDuration(peerKeepalive),
		}
		if len(p.Endpoints) > 0 {
			ep := p.Endpoints[0]
			pc.Endpoint = &net.UDPAddr{IP: ep.Addr().AsSlice(), Port: int(ep.Port())}
		}
		peerCfgs = append(peerCfgs, pc)
		desired[p.PublicKey.String()] = struct{}{}
	}

	for _, current := range dev.Peers {
		if _, ok := desired[current.PublicKey.String()]; ok {
			continue
		}
		peerCfgs = append(peerCfgs, wgtypes.PeerConfig{PublicKey: current.PublicKey, Remove: true})
	}

	return peerCfgs
}

func syncAddresses(link netlink.Link, prefixes []netip.Prefix) error {
	desired := make(map[string]netip.Prefix, len(prefixes))
	for _, pref := range prefixes {
		if !pref.IsValid() {
			continue
		}
		desired[pref.String()] = pref
		addr := &netlink.Addr{IPNet: ptrIPNet(prefixToIPNet(pref))}
		if err := netlink.AddrAdd(link, addr); err != nil && !errors.Is(err, unix.EEXIST) {
			return fmt.Errorf("set wireguard address %s: %w", pref, err)
		}
	}

	addrs, err := netlink.AddrList(link, netlink.FAMILY_ALL)
	if err != nil {
		return fmt.Errorf("list wireguard addresses on %s: %w", link.Attrs().Name, err)
	}
	for _, addr := range addrs {
		if addr.IPNet == nil {
			continue
		}
		pref, err := ipNetToPrefix(*addr.IPNet)
		if err != nil {
			continue
		}
		if _, ok := desired[pref.String()]; ok {
			continue
		}
		if err := netlink.AddrDel(link, &addr); err != nil && !errors.Is(err, unix.EADDRNOTAVAIL) {
			return fmt.Errorf("remove stale wireguard address %s: %w", pref, err)
		}
	}
	return nil
}

func syncRoutes(link netlink.Link, localMachineIP, localMgmtIP netip.Addr, peers []ployz.MachineRecord) error {
	desired := make(map[string]netip.Prefix, len(peers))
	for _, p := range peers {
		if p.OverlayIP.IsValid() {
			pref := overlayPrefix(p.OverlayIP)
			desired[pref.String()] = pref
		}
	}

	for _, pref := range desired {
		r := netlink.Route{LinkIndex: link.Attrs().Index, Scope: netlink.SCOPE_LINK, Dst: ptrIPNet(prefixToIPNet(pref))}
		if pref.Addr().Is4() && localMachineIP.IsValid() && localMachineIP.Is4() {
			r.Src = localMachineIP.AsSlice()
		}
		if err := netlink.RouteReplace(&r); err != nil {
			return fmt.Errorf("set route %s via %s: %w", pref, link.Attrs().Name, err)
		}
	}

	preserve := make(map[string]struct{}, 2)
	if localMachineIP.IsValid() {
		preserve[singleIPPrefix(localMachineIP).String()] = struct{}{}
	}
	if localMgmtIP.IsValid() {
		preserve[singleIPPrefix(localMgmtIP).String()] = struct{}{}
	}

	routes, err := netlink.RouteList(link, netlink.FAMILY_ALL)
	if err != nil {
		return fmt.Errorf("list routes on %s: %w", link.Attrs().Name, err)
	}
	for _, route := range routes {
		if route.Dst == nil || route.Scope != netlink.SCOPE_LINK {
			continue
		}
		pref, err := ipNetToPrefix(*route.Dst)
		if err != nil {
			continue
		}
		if _, ok := preserve[pref.String()]; ok {
			continue
		}
		if _, ok := desired[pref.String()]; ok {
			continue
		}
		if err := netlink.RouteDel(&route); err != nil {
			return fmt.Errorf("remove stale route %s via %s: %w", pref, link.Attrs().Name, err)
		}
	}

	return nil
}

func ptrDuration(d time.Duration) *time.Duration { return &d }
func ptrIPNet(n net.IPNet) *net.IPNet             { return &n }

func singleIPPrefix(addr netip.Addr) netip.Prefix {
	if addr.Is6() {
		return netip.PrefixFrom(addr, 128)
	}
	return netip.PrefixFrom(addr, 32)
}

func prefixToIPNet(pref netip.Prefix) net.IPNet {
	bits := 32
	if pref.Addr().Is6() {
		bits = 128
	}
	return net.IPNet{IP: pref.Addr().AsSlice(), Mask: net.CIDRMask(pref.Bits(), bits)}
}

func ipNetToPrefix(n net.IPNet) (netip.Prefix, error) {
	a, ok := netip.AddrFromSlice(n.IP)
	if !ok {
		return netip.Prefix{}, fmt.Errorf("invalid IP %v", n.IP)
	}
	one, _ := n.Mask.Size()
	return netip.PrefixFrom(a.Unmap(), one), nil
}
