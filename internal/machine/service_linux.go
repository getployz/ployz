//go:build linux

package machine

import (
	"context"
	"errors"
	"fmt"
	"net"
	"net/netip"
	"os"
	"time"

	"ployz/internal/machine/dockerutil"

	"github.com/containerd/errdefs"
	dockernetwork "github.com/docker/docker/api/types/network"
	"github.com/docker/docker/client"
	"github.com/docker/docker/libnetwork/iptables"
	"github.com/vishvananda/netlink"
	"golang.org/x/sys/unix"
	"golang.zx2c4.com/wireguard/wgctrl"
	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

func New() (*Controller, error) {
	cli, err := client.NewClientWithOpts(client.FromEnv, client.WithAPIVersionNegotiation())
	if err != nil {
		return nil, fmt.Errorf("create docker client: %w", err)
	}
	return &Controller{cli: cli}, nil
}

type linuxRuntimeOps struct {
	ctrl *Controller
}

func (o linuxRuntimeOps) Prepare(ctx context.Context, _ Config) error {
	return waitDockerReady(ctx, o.ctrl.cli)
}

func (o linuxRuntimeOps) ConfigureWireGuard(_ context.Context, cfg Config, state *State) error {
	return configureWireGuard(cfg, state, nil)
}

func (o linuxRuntimeOps) EnsureDockerNetwork(ctx context.Context, cfg Config, _ *State) error {
	return ensureDockerNetwork(ctx, o.ctrl.cli, cfg)
}

func (o linuxRuntimeOps) CleanupDockerNetwork(ctx context.Context, cfg Config, state *State) error {
	return cleanupDockerNetwork(ctx, o.ctrl.cli, cfg, state)
}

func (o linuxRuntimeOps) CleanupWireGuard(_ context.Context, _ Config, state *State) error {
	return cleanupWireGuard(state.WGInterface)
}

func (o linuxRuntimeOps) AfterStop(_ context.Context, _ Config, _ *State) error {
	return nil
}

func (c *Controller) Start(ctx context.Context, in Config) (Config, error) {
	return c.startRuntime(ctx, in, linuxRuntimeOps{ctrl: c})
}

func (c *Controller) Stop(ctx context.Context, in Config, purge bool) (Config, error) {
	return c.stopRuntime(ctx, in, purge, linuxRuntimeOps{ctrl: c})
}

func (c *Controller) Status(ctx context.Context, in Config) (Status, error) {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return Status{}, err
	}

	out := Status{StatePath: statePath(cfg.DataDir)}
	s, err := loadState(cfg.DataDir)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return out, nil
		}
		return Status{}, err
	}
	out.Configured = true
	out.Running = s.Running
	cfg, err = Resolve(cfg, s)
	if err != nil {
		return Status{}, err
	}

	if _, err := netlink.LinkByName(s.WGInterface); err == nil {
		out.WireGuard = true
	}
	if err := waitDockerReady(ctx, c.cli); err == nil {
		if n, err := c.cli.NetworkInspect(ctx, s.DockerNetwork, dockernetwork.InspectOptions{}); err == nil && n.ID != "" {
			out.DockerNet = true
		}
		if ctr, err := c.cli.ContainerInspect(ctx, s.CorrosionName); err == nil && ctr.State != nil && ctr.State.Running {
			out.Corrosion = true
		}
	}

	return out, nil
}

func (c *Controller) applyPeerConfig(_ context.Context, cfg Config, state *State, peers []Peer) error {
	return configureWireGuard(cfg, state, peers)
}

func configureWireGuard(cfg Config, state *State, peers []Peer) error {
	link, err := netlink.LinkByName(state.WGInterface)
	if err != nil {
		if _, ok := err.(netlink.LinkNotFoundError); !ok {
			return fmt.Errorf("find wireguard interface %q: %w", state.WGInterface, err)
		}
		link = &netlink.GenericLink{LinkAttrs: netlink.LinkAttrs{Name: state.WGInterface}, LinkType: "wireguard"}
		if err := netlink.LinkAdd(link); err != nil {
			return fmt.Errorf("create wireguard interface %q: %w", state.WGInterface, err)
		}
		link, err = netlink.LinkByName(state.WGInterface)
		if err != nil {
			return fmt.Errorf("refetch wireguard interface %q: %w", state.WGInterface, err)
		}
	}
	if link.Attrs().MTU != defaultWireGuardMTU {
		if err := netlink.LinkSetMTU(link, defaultWireGuardMTU); err != nil {
			return fmt.Errorf("set wireguard mtu on %q: %w", state.WGInterface, err)
		}
	}

	priv, err := wgtypes.ParseKey(state.WGPrivate)
	if err != nil {
		return fmt.Errorf("parse private key: %w", err)
	}

	specs, err := buildPeerSpecs(peers)
	if err != nil {
		return fmt.Errorf("build peer specs: %w", err)
	}

	wg, err := wgctrl.New()
	if err != nil {
		return fmt.Errorf("create wireguard client: %w", err)
	}
	defer wg.Close()

	dev, err := wg.Device(state.WGInterface)
	if err != nil {
		return fmt.Errorf("inspect wireguard device: %w", err)
	}

	peerCfgs := make([]wgtypes.PeerConfig, 0, len(specs))
	desired := make(map[string]struct{}, len(specs))
	for _, spec := range specs {
		pc := wgtypes.PeerConfig{
			PublicKey:                   spec.publicKey,
			ReplaceAllowedIPs:           true,
			AllowedIPs:                  spec.allowedIPNets,
			PersistentKeepaliveInterval: ptrDuration(peerKeepalive),
		}
		if spec.endpoint != nil {
			pc.Endpoint = &net.UDPAddr{IP: spec.endpoint.Addr().AsSlice(), Port: int(spec.endpoint.Port())}
		}
		peerCfgs = append(peerCfgs, pc)
		desired[spec.publicKey.String()] = struct{}{}
	}
	for _, current := range dev.Peers {
		if _, ok := desired[current.PublicKey.String()]; ok {
			continue
		}
		peerCfgs = append(peerCfgs, wgtypes.PeerConfig{PublicKey: current.PublicKey, Remove: true})
	}

	wgCfg := wgtypes.Config{
		PrivateKey:   &priv,
		ListenPort:   &state.WGPort,
		ReplacePeers: false,
		Peers:        peerCfgs,
	}
	if err := wg.ConfigureDevice(state.WGInterface, wgCfg); err != nil {
		return fmt.Errorf("configure wireguard device: %w", err)
	}

	localMachineIP := machineIP(cfg.Subnet)
	if err := syncWireGuardAddresses(link, []netip.Prefix{singleIPPrefix(localMachineIP), singleIPPrefix(cfg.Management)}); err != nil {
		return err
	}
	if link.Attrs().Flags&unix.IFF_UP == 0 {
		if err := netlink.LinkSetUp(link); err != nil {
			return fmt.Errorf("set wireguard interface up: %w", err)
		}
	}

	if err := syncPeerRoutes(link, localMachineIP, cfg.Management, specs); err != nil {
		return err
	}
	return nil
}

func syncWireGuardAddresses(link netlink.Link, prefixes []netip.Prefix) error {
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

func syncPeerRoutes(link netlink.Link, localMachineIP, localMgmtIP netip.Addr, specs []peerSpec) error {
	desired := make(map[string]netip.Prefix)
	for _, spec := range specs {
		for _, pref := range spec.allowedPrefixes {
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

func ptrDuration(d time.Duration) *time.Duration {
	return &d
}

func ptrIPNet(n net.IPNet) *net.IPNet {
	return &n
}

func cleanupWireGuard(iface string) error {
	link, err := netlink.LinkByName(iface)
	if err != nil {
		if _, ok := err.(netlink.LinkNotFoundError); ok {
			return nil
		}
		return fmt.Errorf("find wireguard interface %q: %w", iface, err)
	}
	if err := netlink.LinkDel(link); err != nil {
		return fmt.Errorf("delete wireguard interface %q: %w", iface, err)
	}
	return nil
}

func ensureDockerNetwork(ctx context.Context, cli *client.Client, cfg Config) error {
	needsCreate := false
	nw, err := cli.NetworkInspect(ctx, cfg.DockerNetwork, dockernetwork.InspectOptions{})
	if err != nil {
		if !errdefs.IsNotFound(err) {
			return fmt.Errorf("inspect docker network %q: %w", cfg.DockerNetwork, err)
		}
		needsCreate = true
	} else if len(nw.IPAM.Config) == 0 || nw.IPAM.Config[0].Subnet != cfg.Subnet.String() {
		if err := dockerutil.PurgeNetworkContainers(ctx, cli, cfg.DockerNetwork, nw); err != nil {
			return err
		}
		if err := cli.NetworkRemove(ctx, cfg.DockerNetwork); err != nil {
			return fmt.Errorf("remove old docker network %q: %w", cfg.DockerNetwork, err)
		}
		needsCreate = true
	}

	if needsCreate {
		if _, err := cli.NetworkCreate(ctx, cfg.DockerNetwork, dockernetwork.CreateOptions{
			Driver: "bridge",
			Scope:  "local",
			IPAM:   &dockernetwork.IPAM{Config: []dockernetwork.IPAMConfig{{Subnet: cfg.Subnet.String()}}},
			Options: map[string]string{
				"com.docker.network.bridge.trusted_host_interfaces": cfg.WGInterface,
			},
		}); err != nil {
			return fmt.Errorf("create docker network %q: %w", cfg.DockerNetwork, err)
		}
		nw, err = cli.NetworkInspect(ctx, cfg.DockerNetwork, dockernetwork.InspectOptions{})
		if err != nil {
			return fmt.Errorf("inspect docker network %q: %w", cfg.DockerNetwork, err)
		}
	}

	bridge := "br-" + nw.ID[:12]
	return ensureIptablesRules(cfg, bridge)
}

func cleanupDockerNetwork(ctx context.Context, cli *client.Client, cfg Config, state *State) error {
	nw, err := cli.NetworkInspect(ctx, cfg.DockerNetwork, dockernetwork.InspectOptions{})
	if err != nil {
		if errdefs.IsNotFound(err) {
			return nil
		}
		return fmt.Errorf("inspect docker network %q: %w", cfg.DockerNetwork, err)
	}
	if err := dockerutil.PurgeNetworkContainers(ctx, cli, cfg.DockerNetwork, nw); err != nil {
		return err
	}
	bridge := "br-" + nw.ID[:12]
	if err := cleanupIptablesRules(state, bridge); err != nil {
		return err
	}
	if err := cli.NetworkRemove(ctx, cfg.DockerNetwork); err != nil && !errdefs.IsNotFound(err) {
		return fmt.Errorf("remove docker network %q: %w", cfg.DockerNetwork, err)
	}
	return nil
}

func ensureIptablesRules(cfg Config, bridge string) error {
	ipt := iptables.GetIptable(iptables.IPv4)
	wgRule := []string{"--in-interface", cfg.WGInterface, "--out-interface", bridge, "-j", "ACCEPT"}
	if err := ipt.ProgramRule(iptables.Filter, "DOCKER-USER", iptables.Insert, wgRule); err != nil {
		return fmt.Errorf("insert DOCKER-USER rule: %w", err)
	}
	skipMasq := []string{"--src", cfg.Subnet.String(), "--out-interface", cfg.WGInterface, "-j", "RETURN"}
	_ = ipt.ProgramRule(iptables.Nat, "POSTROUTING", iptables.Delete, skipMasq)
	if err := ipt.ProgramRule(iptables.Nat, "POSTROUTING", iptables.Insert, skipMasq); err != nil {
		return fmt.Errorf("insert POSTROUTING rule: %w", err)
	}
	return nil
}

func cleanupIptablesRules(state *State, bridge string) error {
	ipt := iptables.GetIptable(iptables.IPv4)
	wgRule := []string{"--in-interface", state.WGInterface, "--out-interface", bridge, "-j", "ACCEPT"}
	if err := ipt.ProgramRule(iptables.Filter, "DOCKER-USER", iptables.Delete, wgRule); err != nil {
		return fmt.Errorf("delete DOCKER-USER rule: %w", err)
	}
	subnet, err := netip.ParsePrefix(state.Subnet)
	if err != nil {
		return fmt.Errorf("parse subnet from state: %w", err)
	}
	skipMasq := []string{"--src", subnet.String(), "--out-interface", state.WGInterface, "-j", "RETURN"}
	if err := ipt.ProgramRule(iptables.Nat, "POSTROUTING", iptables.Delete, skipMasq); err != nil {
		return fmt.Errorf("delete POSTROUTING rule: %w", err)
	}
	return nil
}
