package machine

import (
	"context"
	"errors"
	"fmt"
	"net/netip"
	"runtime"
	"strings"
	"time"

	"ployz/internal/buildinfo"
	"ployz/internal/check"
	"ployz/internal/remote"
	"ployz/pkg/ipam"
	"ployz/pkg/sdk/client"
	"ployz/pkg/sdk/defaults"
	"ployz/pkg/sdk/telemetry"
	"ployz/pkg/sdk/types"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/trace"
)

const (
	remoteDaemonSocketPath = "/var/run/ployzd.sock"
	remoteLinuxDataRoot    = "/var/lib/ployz/networks"
	// addWaitTimeout is 45s: allows time for remote install, WireGuard handshake, and supervisor sync.
	addWaitTimeout = 45 * time.Second
	// supervisorSyncPollInterval is 500ms: fast enough to detect sync quickly, slow enough to avoid hammering the API.
	supervisorSyncPollInterval = 500 * time.Millisecond
)

type Service struct {
	api    client.API
	tracer trace.Tracer
}

func New(api client.API) *Service {
	check.Assert(api != nil, "machine.New: API client must not be nil")
	return &Service{api: api, tracer: otel.Tracer("ployz/sdk/machine")}
}

func (s *Service) Start(ctx context.Context, spec types.NetworkSpec) (types.ApplyResult, error) {
	return s.api.ApplyNetworkSpec(ctx, spec)
}

func (s *Service) Stop(ctx context.Context, purge bool) error {
	return s.api.DisableNetwork(ctx, purge)
}

func (s *Service) Status(ctx context.Context) (types.NetworkStatus, error) {
	return s.api.GetStatus(ctx)
}

func (s *Service) Identity(ctx context.Context) (types.Identity, error) {
	return s.api.GetIdentity(ctx)
}

func (s *Service) ListMachines(ctx context.Context) ([]types.MachineEntry, error) {
	return s.api.ListMachines(ctx)
}

func (s *Service) GetPeerHealth(ctx context.Context) ([]types.PeerHealthResponse, error) {
	return s.api.GetPeerHealth(ctx)
}

func (s *Service) RemoveMachine(ctx context.Context, machineID string) error {
	return s.api.RemoveMachine(ctx, machineID)
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

	Tracer trace.Tracer
}

type AddResult struct {
	Machine types.MachineEntry
	Peers   int
}

func (s *Service) AddMachine(ctx context.Context, opts AddOptions) (AddResult, error) {
	tracer := opts.Tracer
	if tracer == nil {
		tracer = s.tracer
	}
	op, err := telemetry.EmitPlan(ctx, tracer, "machine.add", telemetry.Plan{Steps: []telemetry.PlannedStep{
		{ID: "install", Title: "installing ployz on remote"},
		{ID: "connect", Title: "connecting to remote daemon"},
		{ID: "configure", Title: "configuring network"},
		{ID: "register", Title: "registering node in cluster"},
		{ID: "sync", Title: "waiting for supervisor sync"},
	}})
	if err != nil {
		return AddResult{}, err
	}

	var opErr error
	defer func() {
		op.End(opErr)
	}()

	add, err := newAddOp(op.Context(), s.api, opts, op)
	if err != nil {
		opErr = err
		return AddResult{}, err
	}
	defer add.close()

	result, err := add.run()
	opErr = err
	if err != nil {
		return AddResult{}, err
	}
	return result, nil
}

// addOp holds shared state for the AddMachine workflow steps.
type addOp struct {
	ctx           context.Context
	api           client.API
	opts          AddOptions
	phase         AddPhase
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
	op            *telemetry.Operation
}

func newAddOp(ctx context.Context, api client.API, opts AddOptions, op *telemetry.Operation) (*addOp, error) {
	check.Assert(op != nil, "machine.newAddOp: operation must not be nil")

	network := defaults.NormalizeNetwork(opts.Network)
	target := strings.TrimSpace(opts.Target)
	if target == "" && opts.ConnectFunc == nil {
		return nil, fmt.Errorf("target is required")
	}
	if opts.WGPort == 0 {
		opts.WGPort = defaults.WGPort(network)
	}

	localIdentity, err := api.GetIdentity(ctx)
	if err != nil {
		return nil, err
	}
	localMachines, err := api.ListMachines(ctx)
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
		phase:         AddInstall,
		network:       network,
		target:        target,
		remoteEP:      remoteEP,
		networkCIDR:   networkCIDR,
		remoteSubnet:  remoteSubnet,
		remoteRoot:    remoteRoot,
		bootstrap:     collectBootstrapAddrs(localMachines, localMgmtIP, gossipPort),
		localIdentity: localIdentity,
		localMachines: localMachines,
		op:            op,
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
		fn func(context.Context) error
	}{
		{"install", a.install},
		{"connect", a.connect},
		{"configure", a.configure},
		{"register", a.register},
		{"sync", a.syncSupervisor},
	}
	for _, s := range steps {
		err := a.op.RunStep(a.ctx, s.id, s.fn)
		if err != nil {
			a.phase = a.phase.Transition(AddFailed)
			return AddResult{}, err
		}
		a.advancePhase(s.id)
	}

	machines, err := a.api.ListMachines(a.ctx)
	if err != nil {
		a.phase = a.phase.Transition(AddFailed)
		return AddResult{}, err
	}

	result := AddResult{Machine: a.entry, Peers: len(machines)}
	return result, nil
}

func (a *addOp) advancePhase(stepID string) {
	switch stepID {
	case "install":
		a.phase = a.phase.Transition(AddConnect)
	case "connect":
		a.phase = a.phase.Transition(AddConfigure)
	case "configure":
		a.phase = a.phase.Transition(AddRegister)
	case "register":
		a.phase = a.phase.Transition(AddSync)
	case "sync":
		a.phase = a.phase.Transition(AddDone)
	}
}

func (a *addOp) install(ctx context.Context) error {
	if a.opts.ConnectFunc != nil {
		return nil // skip install when using injected connection
	}
	sshOpts := remote.SSHOptions{Port: a.opts.SSHPort, KeyPath: a.opts.SSHKey}
	return remote.RunScript(ctx, a.target, sshOpts, remote.InstallScript(buildinfo.Version))
}

func (a *addOp) connect(ctx context.Context) error {
	if a.opts.ConnectFunc != nil {
		api, err := a.opts.ConnectFunc(ctx)
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

func (a *addOp) configure(ctx context.Context) error {
	if _, err := a.remoteAPI.ApplyNetworkSpec(ctx, types.NetworkSpec{
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

	id, err := a.remoteAPI.GetIdentity(ctx)
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

func (a *addOp) register(ctx context.Context) error {
	return upsertMachineWithRetry(ctx, a.api, &a.entry)
}

func (a *addOp) syncSupervisor(ctx context.Context) error {
	waitCtx, cancel := context.WithTimeout(ctx, addWaitTimeout)
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
	if err := upsertMachineWithRetry(ctx, a.remoteAPI, &localEntry); err != nil {
		return fmt.Errorf("seed local machine on remote: %w", err)
	}
	if err := waitForMachine(waitCtx, a.api, a.entry.ID, "local"); err != nil {
		return err
	}
	return waitForMachine(waitCtx, a.remoteAPI, a.localIdentity.ID, "remote")
}

func waitForMachine(ctx context.Context, api client.API, machineID, who string) error {
	ticker := time.NewTicker(supervisorSyncPollInterval)
	defer ticker.Stop()

	machineID = strings.TrimSpace(machineID)
	if machineID == "" {
		return fmt.Errorf("wait for %s daemon supervisor sync: machine id is required", who)
	}

	var lastSeen int
	for {
		select {
		case <-ctx.Done():
			return fmt.Errorf("wait for %s daemon supervisor sync: %w (visible machines: %d, waiting for %s)", who, ctx.Err(), lastSeen, machineID)
		case <-ticker.C:
			machines, err := api.ListMachines(ctx)
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

func upsertMachineWithRetry(ctx context.Context, api client.API, entry *types.MachineEntry) error {
	if err := api.UpsertMachine(ctx, *entry); err == nil {
		return nil
	} else if !errors.Is(err, client.ErrConflict) {
		return err
	}

	latest, err := api.ListMachines(ctx)
	if err != nil {
		return err
	}
	entry.ExpectedVersion = findExpectedVersion(latest, entry.ID, entry.Endpoint)
	return api.UpsertMachine(ctx, *entry)
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
