package machine

import (
	"context"
	"errors"
	"fmt"
	"net/netip"
	"os/exec"
	"runtime"
	"strings"
	"time"

	"ployz/internal/buildinfo"
	"ployz/internal/check"
	"ployz/internal/remote"
	"ployz/pkg/ipam"
	"ployz/pkg/sdk/client"
	"ployz/pkg/sdk/defaults"
	"ployz/pkg/sdk/progress"
	"ployz/pkg/sdk/types"
)

const (
	remoteDaemonSocketPath = "/var/run/ployzd.sock"
	remoteLinuxDataRoot    = "/var/lib/ployz/networks"
	// addWaitTimeout is 45s: allows time for remote install, WireGuard handshake, and Corrosion gossip convergence.
	addWaitTimeout         = 45 * time.Second
	// convergencePollInterval is 500ms: fast enough to detect convergence quickly, slow enough to avoid hammering the API.
	convergencePollInterval = 500 * time.Millisecond
	// persistentKeepaliveIntervalSec is 25: keeps NAT mappings alive; standard WireGuard recommendation.
	persistentKeepaliveIntervalSec = 25
)

type Service struct {
	api client.API
}

func New(api client.API) *Service {
	check.Assert(api != nil, "machine.New: API client must not be nil")
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

func (s *Service) GetPeerHealth(ctx context.Context, network string) ([]types.PeerHealthResponse, error) {
	return s.api.GetPeerHealth(ctx, network)
}

func (s *Service) RemoveMachine(ctx context.Context, network, machineID string) error {
	if err := s.api.RemoveMachine(ctx, network, machineID); err != nil {
		return err
	}
	return s.api.TriggerReconcile(ctx, network)
}

func (s *Service) HostAccessEndpoint(ctx context.Context, network string) (netip.AddrPort, error) {
	id, err := s.identityForHostAccess(ctx, network)
	if err != nil {
		return netip.AddrPort{}, err
	}
	helperIP, err := helperIPv4(ctx, id.HelperName)
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
	id, err := s.identityForHostAccess(ctx, network)
	if err != nil {
		return err
	}
	if strings.TrimSpace(id.WGInterface) == "" {
		return fmt.Errorf("missing wireguard interface in daemon identity")
	}
	if !hostIP.IsValid() {
		return fmt.Errorf("host ip is required")
	}

	hostCIDR := hostIP.String() + "/32"
	script := fmt.Sprintf(
		`set -eu; wg set %q peer %q persistent-keepalive %d allowed-ips %q; ip route replace %q dev %q`,
		id.WGInterface,
		hostPublicKey,
		persistentKeepaliveIntervalSec,
		hostCIDR,
		hostCIDR,
		id.WGInterface,
	)
	return runDockerExecScript(ctx, id.HelperName, script)
}

func (s *Service) RemoveHostAccessPeer(ctx context.Context, network, hostPublicKey string, hostIP netip.Addr) error {
	id, err := s.identityForHostAccess(ctx, network)
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
	return runDockerExecScript(ctx, id.HelperName, script)
}

type AddOptions struct {
	Network  string
	DataRoot string

	Target   string
	Endpoint string
	SSHPort  int
	SSHKey   string
	WGPort   int

	// ConnectFunc overrides SSH-based remote connection. When set,
	// install() is skipped and connect() calls this instead. If the
	// returned API implements io.Closer, it is closed on cleanup.
	ConnectFunc func(ctx context.Context) (client.API, error)

	OnProgress progress.Reporter
}

type AddResult struct {
	Machine types.MachineEntry
	Peers   int
}

func (s *Service) AddMachine(ctx context.Context, opts AddOptions) (AddResult, error) {
	op, err := newAddOp(ctx, s.api, opts)
	if err != nil {
		return AddResult{}, err
	}
	defer op.close()
	return op.run()
}

// addOp holds shared state for the AddMachine workflow steps.
type addOp struct {
	ctx           context.Context
	api           client.API
	opts          AddOptions
	network       string
	target        string
	remoteEP      string
	networkCIDR   netip.Prefix
	remoteSubnet  netip.Prefix
	remoteRoot    string
	bootstrap     []string
	localIdentity types.Identity
	localMachines []types.MachineEntry
	remoteAPI     client.API
	remoteCloser  func()
	entry         types.MachineEntry
	tracker       *progress.Tracker
}

func newAddOp(ctx context.Context, api client.API, opts AddOptions) (*addOp, error) {
	network := defaults.NormalizeNetwork(opts.Network)
	target := strings.TrimSpace(opts.Target)
	if target == "" && opts.ConnectFunc == nil {
		return nil, fmt.Errorf("target is required")
	}
	if opts.WGPort == 0 {
		opts.WGPort = defaults.WGPort(network)
	}

	localIdentity, err := api.GetIdentity(ctx, network)
	if err != nil {
		return nil, err
	}
	localMachines, err := api.ListMachines(ctx, network)
	if err != nil {
		return nil, err
	}

	var remoteEP string
	if target != "" {
		remoteEP, err = resolveAdvertiseEndpoint(target, opts.Endpoint, opts.WGPort)
		if err != nil {
			return nil, err
		}
	} else {
		remoteEP = strings.TrimSpace(opts.Endpoint)
	}

	networkCIDR, err := netip.ParsePrefix(strings.TrimSpace(localIdentity.NetworkCIDR))
	if err != nil {
		return nil, fmt.Errorf("parse local network cidr: %w", err)
	}

	remoteSubnet, err := chooseRemoteSubnet(networkCIDR, localMachines, remoteEP)
	if err != nil {
		return nil, err
	}

	gossipPort := localIdentity.CorrosionGossipPort
	if gossipPort == 0 {
		gossipPort = defaults.CorrosionGossipPort(network)
	}
	localMgmtIP, err := netip.ParseAddr(strings.TrimSpace(localIdentity.ManagementIP))
	if err != nil {
		return nil, fmt.Errorf("parse local management ip: %w", err)
	}

	remoteRoot := remoteDataRoot(opts.DataRoot)
	if opts.ConnectFunc == nil && remoteRoot != remoteLinuxDataRoot {
		return nil, fmt.Errorf("remote service mode currently supports data root %q only", remoteLinuxDataRoot)
	}

	return &addOp{
		ctx:           ctx,
		api:           api,
		opts:          opts,
		network:       network,
		target:        target,
		remoteEP:      remoteEP,
		networkCIDR:   networkCIDR,
		remoteSubnet:  remoteSubnet,
		remoteRoot:    remoteRoot,
		bootstrap:     collectBootstrapAddrs(localMachines, localMgmtIP, gossipPort),
		localIdentity: localIdentity,
		localMachines: localMachines,
		tracker: progress.New(opts.OnProgress,
			progress.StepConfig{ID: "install", Title: "installing ployz on remote", DoneTitle: "installed ployz on remote", FailedTitle: "failed to install ployz on remote"},
			progress.StepConfig{ID: "connect", Title: "connecting to remote daemon", DoneTitle: "connected to remote daemon", FailedTitle: "failed to connect to remote daemon"},
			progress.StepConfig{ID: "configure", Title: "configuring network", DoneTitle: "configured network", FailedTitle: "failed to configure network"},
			progress.StepConfig{ID: "register", Title: "registering node in cluster", DoneTitle: "registered node in cluster", FailedTitle: "failed to register node in cluster"},
			progress.StepConfig{ID: "converge", Title: "waiting for cluster convergence", DoneTitle: "cluster converged", FailedTitle: "cluster convergence failed"},
		),
	}, nil
}

func (a *addOp) close() {
	if a.remoteCloser != nil {
		a.remoteCloser()
	}
}

func (a *addOp) run() (AddResult, error) {
	steps := []struct {
		id string
		fn func() error
	}{
		{"install", a.install},
		{"connect", a.connect},
		{"configure", a.configure},
		{"register", a.register},
		{"converge", a.converge},
	}
	for _, s := range steps {
		if err := a.tracker.Do(s.id, s.fn); err != nil {
			return AddResult{}, err
		}
	}

	machines, err := a.api.ListMachines(a.ctx, a.network)
	if err != nil {
		return AddResult{}, err
	}
	return AddResult{Machine: a.entry, Peers: len(machines)}, nil
}

func (a *addOp) install() error {
	if a.opts.ConnectFunc != nil {
		return nil // skip install when using injected connection
	}
	sshOpts := remote.SSHOptions{Port: a.opts.SSHPort, KeyPath: a.opts.SSHKey}
	return remote.RunScript(a.ctx, a.target, sshOpts, remote.InstallScript(buildinfo.Version))
}

func (a *addOp) connect() error {
	if a.opts.ConnectFunc != nil {
		api, err := a.opts.ConnectFunc(a.ctx)
		if err != nil {
			return fmt.Errorf("connect to remote daemon: %w", err)
		}
		a.remoteAPI = api
		if closer, ok := api.(interface{ Close() error }); ok {
			a.remoteCloser = func() { _ = closer.Close() }
		}
		return nil
	}
	c, err := client.NewSSH(a.target, client.SSHOptions{
		Port:       a.opts.SSHPort,
		KeyPath:    a.opts.SSHKey,
		SocketPath: remoteDaemonSocketPath,
	})
	if err != nil {
		return fmt.Errorf("connect to remote daemon: %w", err)
	}
	a.remoteAPI = c
	a.remoteCloser = func() { _ = c.Close() }
	return nil
}

func (a *addOp) configure() error {
	if _, err := a.remoteAPI.ApplyNetworkSpec(a.ctx, types.NetworkSpec{
		Network:           a.network,
		DataRoot:          a.remoteRoot,
		NetworkCIDR:       a.networkCIDR.String(),
		Subnet:            a.remoteSubnet.String(),
		WGPort:            a.opts.WGPort,
		CorrosionMemberID: a.localIdentity.CorrosionMemberID,
		CorrosionAPIToken: a.localIdentity.CorrosionAPIToken,
		AdvertiseEndpoint: a.remoteEP,
		Bootstrap:         a.bootstrap,
	}); err != nil {
		return err
	}

	id, err := a.remoteAPI.GetIdentity(a.ctx, a.network)
	if err != nil {
		return err
	}

	a.entry = types.MachineEntry{
		ID:           strings.TrimSpace(id.ID),
		PublicKey:    strings.TrimSpace(id.PublicKey),
		Subnet:       strings.TrimSpace(id.Subnet),
		ManagementIP: strings.TrimSpace(id.ManagementIP),
		Endpoint:     a.remoteEP,
	}
	if a.entry.Subnet == "" {
		a.entry.Subnet = a.remoteSubnet.String()
	}
	a.entry.ExpectedVersion = findExpectedVersion(a.localMachines, a.entry.ID, a.entry.Endpoint)
	return nil
}

func (a *addOp) register() error {
	return upsertMachineWithRetry(a.ctx, a.api, a.network, &a.entry)
}

func (a *addOp) converge() error {
	waitCtx, cancel := context.WithTimeout(a.ctx, addWaitTimeout)
	defer cancel()

	// Seed the local machine entry into the remote's Corrosion directly.
	// Without this, the remote has no way to learn about the local node:
	// Corrosion gossip runs over WireGuard, but the remote won't add the
	// local as a WG peer until it sees it in Corrosion — a chicken-and-egg
	// problem when the local node is behind NAT with no public endpoint.
	localEntry := types.MachineEntry{
		ID:           strings.TrimSpace(a.localIdentity.ID),
		PublicKey:    strings.TrimSpace(a.localIdentity.PublicKey),
		Subnet:       strings.TrimSpace(a.localIdentity.Subnet),
		ManagementIP: strings.TrimSpace(a.localIdentity.ManagementIP),
		Endpoint:     strings.TrimSpace(a.localIdentity.AdvertiseEndpoint),
	}
	localEntry.ExpectedVersion = findExpectedVersion(a.localMachines, localEntry.ID, localEntry.Endpoint)
	if err := upsertMachineWithRetry(a.ctx, a.remoteAPI, a.network, &localEntry); err != nil {
		return fmt.Errorf("seed local machine on remote: %w", err)
	}
	if err := a.api.TriggerReconcile(waitCtx, a.network); err != nil {
		return err
	}
	if err := a.remoteAPI.TriggerReconcile(waitCtx, a.network); err != nil {
		return err
	}
	if err := waitForMachine(waitCtx, a.api, a.network, a.entry.ID, "local"); err != nil {
		return err
	}
	return waitForMachine(waitCtx, a.remoteAPI, a.network, a.localIdentity.ID, "remote")
}

func waitForMachine(ctx context.Context, api client.API, network, machineID, who string) error {
	ticker := time.NewTicker(convergencePollInterval)
	defer ticker.Stop()

	machineID = strings.TrimSpace(machineID)
	if machineID == "" {
		return fmt.Errorf("wait for %s daemon converge: machine id is required", who)
	}

	var lastSeen int
	for {
		select {
		case <-ctx.Done():
			return fmt.Errorf("wait for %s daemon converge: %w (visible machines: %d, waiting for %s)", who, ctx.Err(), lastSeen, machineID)
		case <-ticker.C:
			machines, err := api.ListMachines(ctx, network)
			if err != nil {
				continue
			}
			lastSeen = len(machines)
			for _, m := range machines {
				if strings.TrimSpace(m.ID) == machineID {
					return nil
				}
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
				return m.Version
			}
		}
	}
	if endpoint != "" {
		for _, m := range machines {
			if strings.TrimSpace(m.Endpoint) == endpoint {
				return m.Version
			}
		}
	}
	return 0
}

func collectBootstrapAddrs(machines []types.MachineEntry, fallbackMgmt netip.Addr, gossipPort int) []string {
	seen := make(map[string]struct{})
	bootstrap := make([]string, 0, len(machines)+1)

	appendAddr := func(addr netip.Addr) {
		if !addr.IsValid() {
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
	for _, m := range machines {
		mgmt := strings.TrimSpace(m.ManagementIP)
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
	for _, m := range machines {
		if strings.TrimSpace(m.Endpoint) != remoteEndpoint {
			continue
		}
		subnet, err := netip.ParsePrefix(strings.TrimSpace(m.Subnet))
		if err != nil {
			return netip.Prefix{}, fmt.Errorf("parse existing machine subnet: %w", err)
		}
		return subnet, nil
	}

	allocated := make([]netip.Prefix, 0, len(machines))
	for _, m := range machines {
		subnet, err := netip.ParsePrefix(strings.TrimSpace(m.Subnet))
		if err != nil {
			continue
		}
		allocated = append(allocated, subnet)
	}
	return ipam.AllocateSubnet(networkCIDR, allocated)
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
	if _, after, ok := strings.Cut(target, "@"); ok {
		host = after
	}
	host = strings.TrimSpace(host)
	addr, err := netip.ParseAddr(host)
	if err != nil {
		return "", fmt.Errorf("target host %q is not an IP address; use --endpoint ip:port", host)
	}
	return netip.AddrPortFrom(addr, uint16(wgPort)).String(), nil
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

func (s *Service) identityForHostAccess(ctx context.Context, network string) (types.Identity, error) {
	id, err := s.api.GetIdentity(ctx, network)
	if err != nil {
		return types.Identity{}, err
	}
	id.HelperName = strings.TrimSpace(id.HelperName)
	if id.HelperName == "" {
		id.HelperName = defaults.HelperName(network)
	}
	return id, nil
}

func helperIPv4(ctx context.Context, helperName string) (netip.Addr, error) {
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
	cmd := exec.CommandContext(ctx, "docker", "exec", containerName, "sh", "-c", script)
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
