package machine

import (
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"math"
	"net/netip"
	"os/exec"
	"runtime"
	"strings"
	"time"

	"ployz/internal/machine/remote"
	"ployz/pkg/sdk/client"
	"ployz/pkg/sdk/defaults"
	"ployz/pkg/sdk/types"
)

const (
	remoteDaemonSocketPath = "/var/run/ployzd.sock"
	remoteLinuxDataRoot    = "/var/lib/ployz/networks"
	addWaitTimeout         = 45 * time.Second
	machineSubnetBits      = 24
)

type Service struct {
	api client.API
}

func New(api client.API) *Service {
	return &Service{api: api}
}

func (s *Service) Start(ctx context.Context, spec types.NetworkSpec) (types.ApplyResult, error) {
	return s.api.ApplyNetworkSpec(ctx, spec)
}

func (s *Service) Stop(ctx context.Context, network string, purge bool) error {
	return s.api.DisableNetwork(ctx, network, purge)
}

func (s *Service) Status(ctx context.Context, network string) (types.NetworkStatus, error) {
	return s.api.GetStatus(ctx, network)
}

func (s *Service) Identity(ctx context.Context, network string) (types.Identity, error) {
	return s.api.GetIdentity(ctx, network)
}

func (s *Service) ListMachines(ctx context.Context, network string) ([]types.MachineEntry, error) {
	return s.api.ListMachines(ctx, network)
}

func (s *Service) RemoveMachine(ctx context.Context, network, machineID string) error {
	if err := s.api.RemoveMachine(ctx, network, machineID); err != nil {
		return err
	}
	return s.api.TriggerReconcile(ctx, network)
}

func (s *Service) Reconcile(ctx context.Context, network string) error {
	return s.api.TriggerReconcile(ctx, network)
}

func (s *Service) HostAccessEndpoint(ctx context.Context, network string) (netip.AddrPort, error) {
	id, helperName, err := s.identityForHostAccess(ctx, network)
	if err != nil {
		return netip.AddrPort{}, err
	}
	helperIP, err := helperIPv4(ctx, helperName)
	if err != nil {
		return netip.AddrPort{}, err
	}
	wgPort := id.WGPort
	if wgPort == 0 {
		wgPort = defaults.WGPort(network)
	}
	return netip.AddrPortFrom(helperIP, uint16(wgPort)), nil
}

func (s *Service) AddHostAccessPeer(ctx context.Context, network, hostPublicKey string, hostIP netip.Addr) error {
	id, _, err := s.identityForHostAccess(ctx, network)
	if err != nil {
		return err
	}
	if strings.TrimSpace(id.WGInterface) == "" {
		return fmt.Errorf("missing wireguard interface in daemon identity")
	}
	if !hostIP.IsValid() {
		return fmt.Errorf("host ip is required")
	}

	script := fmt.Sprintf(
		`set -eu; wg set %q peer %q persistent-keepalive 25 allowed-ips %q; ip route replace %q dev %q`,
		id.WGInterface,
		hostPublicKey,
		hostIP.String()+"/32",
		hostIP.String()+"/32",
		id.WGInterface,
	)
	return runDockerExecScript(ctx, strings.TrimSpace(id.HelperName), script)
}

func (s *Service) RemoveHostAccessPeer(ctx context.Context, network, hostPublicKey string, hostIP netip.Addr) error {
	id, _, err := s.identityForHostAccess(ctx, network)
	if err != nil {
		return err
	}
	if strings.TrimSpace(id.WGInterface) == "" {
		return nil
	}

	hostCIDR := ""
	if hostIP.IsValid() {
		hostCIDR = hostIP.String() + "/32"
	}
	script := fmt.Sprintf(
		`set -eu; wg set %q peer %q remove || true; ip route del %q dev %q >/dev/null 2>&1 || true`,
		id.WGInterface,
		hostPublicKey,
		hostCIDR,
		id.WGInterface,
	)
	return runDockerExecScript(ctx, strings.TrimSpace(id.HelperName), script)
}

type AddOptions struct {
	Network  string
	DataRoot string

	Target   string
	Endpoint string
	SSHPort  int
	SSHKey   string
	WGPort   int
}

type AddResult struct {
	Machine types.MachineEntry
	Peers   int
}

func (s *Service) AddMachine(ctx context.Context, opts AddOptions) (AddResult, error) {
	network := normalizeNetwork(opts.Network)
	target := strings.TrimSpace(opts.Target)
	if target == "" {
		return AddResult{}, fmt.Errorf("target is required")
	}
	if opts.WGPort == 0 {
		opts.WGPort = defaults.WGPort(network)
	}

	localIdentity, err := s.api.GetIdentity(ctx, network)
	if err != nil {
		return AddResult{}, err
	}
	localMachines, err := s.api.ListMachines(ctx, network)
	if err != nil {
		return AddResult{}, err
	}

	remoteEP, err := resolveAdvertiseEndpoint(target, opts.Endpoint, opts.WGPort)
	if err != nil {
		return AddResult{}, err
	}

	networkCIDR, err := netip.ParsePrefix(strings.TrimSpace(localIdentity.NetworkCIDR))
	if err != nil {
		return AddResult{}, fmt.Errorf("parse local network cidr: %w", err)
	}

	remoteSubnet, err := chooseRemoteSubnet(networkCIDR, localMachines, remoteEP)
	if err != nil {
		return AddResult{}, err
	}

	gossipPort := localIdentity.CorrosionGossip
	if gossipPort == 0 {
		gossipPort = defaults.CorrosionGossipPort(network)
	}
	localMgmtIP, err := netip.ParseAddr(strings.TrimSpace(localIdentity.ManagementIP))
	if err != nil {
		return AddResult{}, fmt.Errorf("parse local management ip: %w", err)
	}
	bootstrap := collectBootstrapAddrs(localMachines, localMgmtIP, gossipPort)

	sshOpts := remote.SSHOptions{Port: opts.SSHPort, KeyPath: opts.SSHKey}
	if err := remote.RunScript(ctx, target, sshOpts, remote.PreflightScript()); err != nil {
		return AddResult{}, err
	}
	if err := remote.RunScript(ctx, target, sshOpts, remote.EnsureDaemonScript(remoteDaemonSocketPath, remoteDataRoot(opts.DataRoot))); err != nil {
		return AddResult{}, err
	}

	remoteAPI, err := client.NewSSH(target, client.SSHOptions{
		Port:       opts.SSHPort,
		KeyPath:    opts.SSHKey,
		SocketPath: remoteDaemonSocketPath,
	})
	if err != nil {
		return AddResult{}, fmt.Errorf("connect to remote daemon: %w", err)
	}

	if _, err := remoteAPI.ApplyNetworkSpec(ctx, types.NetworkSpec{
		Network:           network,
		DataRoot:          remoteDataRoot(opts.DataRoot),
		NetworkCIDR:       networkCIDR.String(),
		Subnet:            remoteSubnet.String(),
		WGPort:            opts.WGPort,
		AdvertiseEndpoint: remoteEP,
		Bootstrap:         bootstrap,
	}); err != nil {
		return AddResult{}, err
	}

	remoteIdentity, err := remoteAPI.GetIdentity(ctx, network)
	if err != nil {
		return AddResult{}, err
	}

	entry := types.MachineEntry{
		ID:           strings.TrimSpace(remoteIdentity.ID),
		PublicKey:    strings.TrimSpace(remoteIdentity.PublicKey),
		Subnet:       strings.TrimSpace(remoteIdentity.Subnet),
		ManagementIP: strings.TrimSpace(remoteIdentity.ManagementIP),
		Endpoint:     remoteEP,
	}
	if entry.Subnet == "" {
		entry.Subnet = remoteSubnet.String()
	}
	entry.ExpectedVersion = findExpectedVersion(localMachines, entry.ID, entry.Endpoint)

	if err := upsertMachineWithRetry(ctx, s.api, network, &entry); err != nil {
		return AddResult{}, err
	}

	waitCtx, cancel := context.WithTimeout(ctx, addWaitTimeout)
	defer cancel()
	localEvents, err := s.api.StreamEvents(waitCtx, network)
	if err != nil {
		return AddResult{}, err
	}
	remoteEvents, err := remoteAPI.StreamEvents(waitCtx, network)
	if err != nil {
		return AddResult{}, err
	}

	if err := s.api.TriggerReconcile(waitCtx, network); err != nil {
		return AddResult{}, err
	}
	if err := remoteAPI.TriggerReconcile(waitCtx, network); err != nil {
		return AddResult{}, err
	}

	if err := waitForReconcile(waitCtx, "local", localEvents); err != nil {
		return AddResult{}, err
	}
	if err := waitForReconcile(waitCtx, "remote", remoteEvents); err != nil {
		return AddResult{}, err
	}

	machines, err := s.api.ListMachines(ctx, network)
	if err != nil {
		return AddResult{}, err
	}
	return AddResult{Machine: entry, Peers: len(machines)}, nil
}

func waitForReconcile(ctx context.Context, who string, events <-chan types.Event) error {
	for {
		select {
		case <-ctx.Done():
			return fmt.Errorf("wait for %s daemon converge: %w", who, ctx.Err())
		case ev, ok := <-events:
			if !ok {
				return fmt.Errorf("%s daemon event stream closed before convergence", who)
			}
			switch ev.Type {
			case "reconcile.success":
				return nil
			case "reconcile.error", "worker.error":
				if strings.TrimSpace(ev.Message) == "" {
					return fmt.Errorf("%s daemon reported %s", who, ev.Type)
				}
				return fmt.Errorf("%s daemon %s: %s", who, ev.Type, ev.Message)
			}
		}
	}
}

func upsertMachineWithRetry(ctx context.Context, api client.API, network string, entry *types.MachineEntry) error {
	if err := api.UpsertMachine(ctx, network, *entry); err == nil {
		return nil
	} else if !errors.Is(err, client.ErrConflict) {
		return err
	}

	latest, err := api.ListMachines(ctx, network)
	if err != nil {
		return err
	}
	entry.ExpectedVersion = findExpectedVersion(latest, entry.ID, entry.Endpoint)
	return api.UpsertMachine(ctx, network, *entry)
}

func findExpectedVersion(machines []types.MachineEntry, id, endpoint string) int64 {
	id = strings.TrimSpace(id)
	endpoint = strings.TrimSpace(endpoint)
	if id != "" {
		for _, m := range machines {
			if strings.TrimSpace(m.ID) == id {
				if m.Version > 0 {
					return m.Version
				}
				return 0
			}
		}
	}
	if endpoint != "" {
		for _, m := range machines {
			if strings.TrimSpace(m.Endpoint) == endpoint {
				if m.Version > 0 {
					return m.Version
				}
				return 0
			}
		}
	}
	return 0
}

func collectBootstrapAddrs(machines []types.MachineEntry, fallbackMgmt netip.Addr, gossipPort int, exclude ...netip.Addr) []string {
	seen := make(map[string]struct{})
	bootstrap := make([]string, 0, len(machines)+1)
	excluded := make(map[string]struct{}, len(exclude))
	for _, addr := range exclude {
		if !addr.IsValid() {
			continue
		}
		excluded[addr.String()] = struct{}{}
	}

	appendAddr := func(addr netip.Addr) {
		if !addr.IsValid() {
			return
		}
		if _, ok := excluded[addr.String()]; ok {
			return
		}
		addrPort := netip.AddrPortFrom(addr, uint16(gossipPort)).String()
		if _, ok := seen[addrPort]; ok {
			return
		}
		seen[addrPort] = struct{}{}
		bootstrap = append(bootstrap, addrPort)
	}

	appendAddr(fallbackMgmt)
	for _, machine := range machines {
		mgmt := strings.TrimSpace(machine.ManagementIP)
		if mgmt == "" {
			continue
		}
		addr, err := netip.ParseAddr(mgmt)
		if err != nil {
			continue
		}
		appendAddr(addr)
	}

	return bootstrap
}

func chooseRemoteSubnet(networkCIDR netip.Prefix, machines []types.MachineEntry, remoteEndpoint string) (netip.Prefix, error) {
	for _, machine := range machines {
		if strings.TrimSpace(machine.Endpoint) != strings.TrimSpace(remoteEndpoint) {
			continue
		}
		subnet, err := netip.ParsePrefix(strings.TrimSpace(machine.Subnet))
		if err != nil {
			return netip.Prefix{}, fmt.Errorf("parse existing machine subnet: %w", err)
		}
		return subnet, nil
	}

	allocated := make([]netip.Prefix, 0, len(machines))
	for _, machine := range machines {
		subnet, err := netip.ParsePrefix(strings.TrimSpace(machine.Subnet))
		if err != nil {
			continue
		}
		allocated = append(allocated, subnet)
	}
	return allocateSubnet(networkCIDR, allocated)
}

func allocateSubnet(network netip.Prefix, allocated []netip.Prefix) (netip.Prefix, error) {
	if !network.IsValid() {
		return netip.Prefix{}, fmt.Errorf("network cidr is required")
	}
	network = network.Masked()
	if !network.Addr().Is4() {
		return netip.Prefix{}, fmt.Errorf("only ipv4 network cidr is supported")
	}
	if machineSubnetBits < network.Bits() || machineSubnetBits > 32 {
		return netip.Prefix{}, fmt.Errorf("invalid subnet size /%d for network %s", machineSubnetBits, network)
	}

	start, end, err := prefixRange4(network)
	if err != nil {
		return netip.Prefix{}, err
	}
	step := uint32(1) << (32 - machineSubnetBits)

	ranges := make([][2]uint32, 0, len(allocated))
	for _, subnet := range allocated {
		if !subnet.IsValid() || !subnet.Addr().Is4() {
			continue
		}
		if !network.Contains(subnet.Masked().Addr()) {
			continue
		}
		aStart, aEnd, rErr := prefixRange4(subnet)
		if rErr != nil {
			continue
		}
		ranges = append(ranges, [2]uint32{aStart, aEnd})
	}

	for curr := start; curr <= end; {
		subnet := netip.PrefixFrom(uint32ToAddr(curr), machineSubnetBits)
		sStart, sEnd, _ := prefixRange4(subnet)

		overlap := false
		for _, r := range ranges {
			if rangesOverlap(sStart, sEnd, r[0], r[1]) {
				overlap = true
				break
			}
		}
		if !overlap {
			return subnet, nil
		}

		if curr > math.MaxUint32-step {
			break
		}
		curr += step
	}

	return netip.Prefix{}, fmt.Errorf("no available /%d subnet in %s", machineSubnetBits, network)
}

func prefixRange4(prefix netip.Prefix) (uint32, uint32, error) {
	prefix = prefix.Masked()
	if !prefix.Addr().Is4() {
		return 0, 0, fmt.Errorf("prefix %s is not ipv4", prefix)
	}
	b := prefix.Addr().As4()
	start := binary.BigEndian.Uint32(b[:])
	hostBits := 32 - prefix.Bits()
	if hostBits <= 0 {
		return start, start, nil
	}
	if hostBits >= 32 {
		return 0, math.MaxUint32, nil
	}
	size := uint32(1) << hostBits
	return start, start + size - 1, nil
}

func rangesOverlap(aStart, aEnd, bStart, bEnd uint32) bool {
	return !(aEnd < bStart || bEnd < aStart)
}

func uint32ToAddr(v uint32) netip.Addr {
	var b [4]byte
	binary.BigEndian.PutUint32(b[:], v)
	return netip.AddrFrom4(b)
}

func resolveAdvertiseEndpoint(target, override string, wgPort int) (string, error) {
	override = strings.TrimSpace(override)
	if override != "" {
		if _, err := netip.ParseAddrPort(override); err != nil {
			return "", fmt.Errorf("parse endpoint: %w", err)
		}
		return override, nil
	}

	host := target
	if strings.Contains(target, "@") {
		parts := strings.SplitN(target, "@", 2)
		host = parts[1]
	}
	host = strings.TrimSpace(host)
	addr, err := netip.ParseAddr(host)
	if err != nil {
		return "", fmt.Errorf("target host %q is not an IP address; use --endpoint ip:port", host)
	}
	return netip.AddrPortFrom(addr, uint16(wgPort)).String(), nil
}

func normalizeNetwork(network string) string {
	network = strings.TrimSpace(network)
	if network == "" {
		return "default"
	}
	return network
}

func remoteDataRoot(dataRoot string) string {
	dataRoot = strings.TrimSpace(dataRoot)
	if dataRoot == "" {
		return remoteLinuxDataRoot
	}
	if runtime.GOOS == "darwin" && dataRoot == defaults.DataRoot() {
		return remoteLinuxDataRoot
	}
	return dataRoot
}

func (s *Service) identityForHostAccess(ctx context.Context, network string) (types.Identity, string, error) {
	id, err := s.api.GetIdentity(ctx, network)
	if err != nil {
		return types.Identity{}, "", err
	}
	helperName := strings.TrimSpace(id.HelperName)
	if helperName == "" {
		helperName = defaults.HelperName(network)
	}
	id.HelperName = helperName
	return id, helperName, nil
}

func helperIPv4(ctx context.Context, helperName string) (netip.Addr, error) {
	if strings.TrimSpace(helperName) == "" {
		return netip.Addr{}, fmt.Errorf("helper container name is required")
	}
	out, err := runDockerExecScriptOutput(ctx, helperName, `set -eu
ip -4 -o addr show dev eth0 | awk 'NR==1 {print $4}' | cut -d/ -f1`)
	if err != nil {
		return netip.Addr{}, fmt.Errorf("read helper eth0 address: %w", err)
	}
	addr, err := netip.ParseAddr(strings.TrimSpace(out))
	if err != nil {
		return netip.Addr{}, fmt.Errorf("parse helper IPv4 address: %w", err)
	}
	return addr, nil
}

func runDockerExecScript(ctx context.Context, containerName, script string) error {
	_, err := runDockerExecScriptOutput(ctx, containerName, script)
	return err
}

func runDockerExecScriptOutput(ctx context.Context, containerName, script string) (string, error) {
	cmd := exec.CommandContext(ctx, "docker", "exec", containerName, "sh", "-lc", script)
	out, err := cmd.CombinedOutput()
	if err != nil {
		msg := strings.TrimSpace(string(out))
		if msg == "" {
			return "", fmt.Errorf("docker exec %s: %w", containerName, err)
		}
		return "", fmt.Errorf("docker exec %s: %w: %s", containerName, err, msg)
	}
	return strings.TrimSpace(string(out)), nil
}
