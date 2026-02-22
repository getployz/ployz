package server

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/netip"
	"os"
	"os/user"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"

	pb "ployz/internal/daemon/pb"
	proxymod "ployz/internal/daemon/proxy"
	"ployz/internal/daemon/supervisor"
	"ployz/internal/mesh"
	"ployz/pkg/sdk/defaults"
	"ployz/pkg/sdk/types"

	grpcproxy "github.com/siderolabs/grpc-proxy/proxy"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

const (
	// identityPollInterval is 2s: balances responsiveness with CPU cost when waiting for first network setup.
	identityPollInterval = 2 * time.Second
	// serverGoroutineCount is 3: direct gRPC server + proxy server + TCP identity watcher.
	serverGoroutineCount = 3
)

type Server struct {
	pb.UnimplementedDaemonServer
	manager *supervisor.Manager
}

func New(manager *supervisor.Manager) *Server {
	return &Server{manager: manager}
}

// ListenAndServe starts the daemon with a proxy layer.
func (s *Server) ListenAndServe(ctx context.Context, socketPath string) error {
	log := slog.With("component", "daemon-server", "socket", socketPath)
	internalSockPath := internalSocketPath(socketPath)

	// Always start the direct gRPC server on the internal socket.
	directSrv := grpc.NewServer()
	pb.RegisterDaemonServer(directSrv, s)

	directLn, err := listenUnix(internalSockPath)
	if err != nil {
		return fmt.Errorf("listen internal socket: %w", err)
	}
	log.Debug("internal listener started", "socket", internalSockPath)
	serveErr := make(chan error, serverGoroutineCount)
	go func() { serveErr <- directSrv.Serve(directLn) }()

	// Create the proxy director.
	mapper := proxyMapper{manager: s.manager}
	remotePort := uint16(defaults.DaemonAPIPort("default"))
	director := proxymod.NewDirector(internalSockPath, remotePort, mapper)

	proxySrv := grpc.NewServer(
		grpc.ForceServerCodecV2(grpcproxy.Codec()),
		grpc.UnknownServiceHandler(grpcproxy.TransparentHandler(director.Director)),
	)

	// Start proxy on the external unix socket (CLI entry point).
	proxyLn, err := listenUnix(socketPath)
	if err != nil {
		directSrv.GracefulStop()
		_ = os.Remove(internalSockPath) // best-effort cleanup
		return fmt.Errorf("listen proxy socket: %w", err)
	}
	log.Debug("proxy listener started", "socket", socketPath)
	go func() { serveErr <- proxySrv.Serve(proxyLn) }()

	// Start a goroutine that watches for the identity to become available,
	// then starts the TCP listener and updates the director.
	go s.watchIdentityAndServeTCP(ctx, director, serveErr)

	var retErr error
	select {
	case <-ctx.Done():
		log.Info("shutting down listeners")
	case retErr = <-serveErr:
		log.Error("listener exited", "err", retErr)
	}

	proxySrv.GracefulStop()
	directSrv.GracefulStop()
	director.Close()
	_ = os.Remove(socketPath)         // best-effort cleanup
	_ = os.Remove(internalSockPath)   // best-effort cleanup
	return retErr
}

func (s *Server) watchIdentityAndServeTCP(ctx context.Context, director *proxymod.Director, serveErr chan<- error) {
	ticker := time.NewTicker(identityPollInterval)
	defer ticker.Stop()

	var identity types.Identity
	for {
		id, err := s.manager.GetIdentity(ctx, "default")
		if err == nil && strings.TrimSpace(id.ManagementIP) != "" {
			identity = id
			break
		}
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
	}

	mgmtIP := strings.TrimSpace(identity.ManagementIP)
	nodeID := strings.TrimSpace(identity.ID)

	director.UpdateLocalMachine(nodeID, mgmtIP)
	slog.Debug("proxy: local identity resolved", "node_id", nodeID[:min(8, len(nodeID))]+"…", "mgmt_ip", mgmtIP)

	port := defaults.DaemonAPIPort("default")
	portStr := strconv.Itoa(port)

	listenAddrs := []string{net.JoinHostPort(mgmtIP, portStr)}
	if prefix, err := netip.ParsePrefix(strings.TrimSpace(identity.Subnet)); err == nil {
		listenAddrs = append(listenAddrs, net.JoinHostPort(mesh.MachineIP(prefix).String(), portStr))
	}

	tcpSrv := grpc.NewServer(
		grpc.ForceServerCodecV2(grpcproxy.Codec()),
		grpc.UnknownServiceHandler(grpcproxy.TransparentHandler(director.Director)),
	)
	go func() {
		<-ctx.Done()
		tcpSrv.GracefulStop()
	}()

	for _, addr := range listenAddrs {
		tcpLn, err := net.Listen("tcp", addr)
		if err != nil {
			slog.Debug("proxy: TCP listen failed (non-fatal)", "addr", addr, "err", err)
			continue
		}
		slog.Debug("proxy: TCP listener started", "addr", addr)
		go func() { serveErr <- tcpSrv.Serve(tcpLn) }()
	}
}

// --- gRPC methods: proto ↔ types conversion boundary ---

func (s *Server) ApplyNetworkSpec(ctx context.Context, req *pb.ApplyNetworkSpecRequest) (*pb.ApplyResult, error) {
	if req.Spec == nil {
		return nil, status.Error(codes.InvalidArgument, "spec is required")
	}
	out, err := s.manager.ApplyNetworkSpec(ctx, specFromProto(req.Spec))
	if err != nil {
		return nil, toGRPCError(err)
	}
	return applyResultToProto(out), nil
}

func (s *Server) DisableNetwork(ctx context.Context, req *pb.DisableNetworkRequest) (*pb.DisableNetworkResponse, error) {
	if err := s.manager.DisableNetwork(ctx, req.Network, req.Purge); err != nil {
		return nil, toGRPCError(err)
	}
	return &pb.DisableNetworkResponse{}, nil
}

func (s *Server) GetStatus(ctx context.Context, req *pb.GetStatusRequest) (*pb.NetworkStatus, error) {
	st, err := s.manager.GetStatus(ctx, req.Network)
	if err != nil {
		return nil, toGRPCError(err)
	}
	return statusToProto(st), nil
}

func (s *Server) GetIdentity(ctx context.Context, req *pb.GetIdentityRequest) (*pb.Identity, error) {
	id, err := s.manager.GetIdentity(ctx, req.Network)
	if err != nil {
		return nil, toGRPCError(err)
	}
	return identityToProto(id), nil
}

func (s *Server) ListMachines(ctx context.Context, req *pb.ListMachinesRequest) (*pb.ListMachinesResponse, error) {
	machines, err := s.manager.ListMachines(ctx, req.Network)
	if err != nil {
		return nil, toGRPCError(err)
	}
	pbMachines := make([]*pb.MachineEntry, len(machines))
	for i, m := range machines {
		pbMachines[i] = machineEntryToProto(m)
	}
	return &pb.ListMachinesResponse{Machines: pbMachines}, nil
}

func (s *Server) UpsertMachine(ctx context.Context, req *pb.UpsertMachineRequest) (*pb.UpsertMachineResponse, error) {
	if req.Machine == nil {
		return nil, status.Error(codes.InvalidArgument, "machine is required")
	}
	if err := s.manager.UpsertMachine(ctx, req.Network, machineEntryFromProto(req.Machine)); err != nil {
		return nil, toGRPCError(err)
	}
	return &pb.UpsertMachineResponse{}, nil
}

func (s *Server) RemoveMachine(ctx context.Context, req *pb.RemoveMachineRequest) (*pb.RemoveMachineResponse, error) {
	if err := s.manager.RemoveMachine(ctx, req.Network, req.IdOrEndpoint); err != nil {
		return nil, toGRPCError(err)
	}
	return &pb.RemoveMachineResponse{}, nil
}

func (s *Server) TriggerReconcile(ctx context.Context, req *pb.TriggerReconcileRequest) (*pb.TriggerReconcileResponse, error) {
	if err := s.manager.TriggerReconcile(ctx, req.Network); err != nil {
		return nil, toGRPCError(err)
	}
	return &pb.TriggerReconcileResponse{}, nil
}

func (s *Server) GetPeerHealth(ctx context.Context, req *pb.GetPeerHealthRequest) (*pb.GetPeerHealthResponse, error) {
	responses, err := s.manager.GetPeerHealth(ctx, req.Network)
	if err != nil {
		return nil, toGRPCError(err)
	}
	return peerHealthToProto(responses), nil
}

// --- proto ↔ types conversion helpers ---

func specFromProto(p *pb.NetworkSpec) types.NetworkSpec {
	return types.NetworkSpec{
		Network:           p.Network,
		DataRoot:          p.DataRoot,
		NetworkCIDR:       p.NetworkCidr,
		Subnet:            p.Subnet,
		ManagementIP:      p.ManagementIp,
		AdvertiseEndpoint: p.AdvertiseEndpoint,
		WGPort:            int(p.WgPort),
		CorrosionMemberID: p.CorrosionMemberId,
		CorrosionAPIToken: p.CorrosionApiToken,
		Bootstrap:         p.Bootstrap,
		HelperImage:       p.HelperImage,
	}
}

func applyResultToProto(r types.ApplyResult) *pb.ApplyResult {
	return &pb.ApplyResult{
		Network:             r.Network,
		NetworkCidr:         r.NetworkCIDR,
		Subnet:              r.Subnet,
		ManagementIp:        r.ManagementIP,
		WgInterface:         r.WGInterface,
		WgPort:              int32(r.WGPort),
		AdvertiseEndpoint:   r.AdvertiseEndpoint,
		CorrosionName:       r.CorrosionName,
		CorrosionApiAddr:    r.CorrosionAPIAddr,
		CorrosionGossipAddr: r.CorrosionGossipAddrPort,
		DockerNetwork:       r.DockerNetwork,
		ConvergenceRunning:  r.ConvergenceRunning,
	}
}

func statusToProto(st types.NetworkStatus) *pb.NetworkStatus {
	return &pb.NetworkStatus{
		Configured:    st.Configured,
		Running:       st.Running,
		Wireguard:     st.WireGuard,
		Corrosion:     st.Corrosion,
		Docker:        st.DockerNet,
		StatePath:     st.StatePath,
		WorkerRunning: st.WorkerRunning,
		ClockHealth: &pb.ClockHealth{
			NtpOffsetMs: st.ClockHealth.NTPOffsetMs,
			NtpHealthy:  st.ClockHealth.NTPHealthy,
			NtpError:    st.ClockHealth.NTPError,
		},
	}
}

func identityToProto(id types.Identity) *pb.Identity {
	return &pb.Identity{
		Id:                  id.ID,
		PublicKey:           id.PublicKey,
		Subnet:              id.Subnet,
		ManagementIp:        id.ManagementIP,
		AdvertiseEndpoint:   id.AdvertiseEndpoint,
		NetworkCidr:         id.NetworkCIDR,
		WgInterface:         id.WGInterface,
		WgPort:              int32(id.WGPort),
		HelperName:          id.HelperName,
		CorrosionGossipPort: int32(id.CorrosionGossipPort),
		CorrosionMemberId:   id.CorrosionMemberID,
		CorrosionApiToken:   id.CorrosionAPIToken,
		Running:             id.Running,
	}
}

func machineEntryToProto(m types.MachineEntry) *pb.MachineEntry {
	return &pb.MachineEntry{
		Id:              m.ID,
		PublicKey:       m.PublicKey,
		Subnet:          m.Subnet,
		ManagementIp:    m.ManagementIP,
		Endpoint:        m.Endpoint,
		LastUpdated:     m.LastUpdated,
		Version:         m.Version,
		ExpectedVersion: m.ExpectedVersion,
		FreshnessMs:     float64(m.Freshness.Milliseconds()),
		Stale:           m.Stale,
		ReplicationLagMs: float64(m.ReplicationLag.Milliseconds()),
	}
}

func machineEntryFromProto(p *pb.MachineEntry) types.MachineEntry {
	return types.MachineEntry{
		ID:              p.Id,
		PublicKey:       p.PublicKey,
		Subnet:          p.Subnet,
		ManagementIP:    p.ManagementIp,
		Endpoint:        p.Endpoint,
		LastUpdated:     p.LastUpdated,
		Version:         p.Version,
		ExpectedVersion: p.ExpectedVersion,
	}
}

func peerHealthToProto(responses []types.PeerHealthResponse) *pb.GetPeerHealthResponse {
	messages := make([]*pb.PeerHealthReply, len(responses))
	for i, r := range responses {
		peers := make([]*pb.PeerLag, len(r.Peers))
		for j, p := range r.Peers {
			var pingMs float64
			switch {
			case p.PingRTT < 0:
				pingMs = -1
			case p.PingRTT > 0:
				pingMs = float64(p.PingRTT.Microseconds()) / 1000.0
			}
			peers[j] = &pb.PeerLag{
				NodeId:           p.NodeID,
				FreshnessMs:      float64(p.Freshness.Milliseconds()),
				Stale:            p.Stale,
				ReplicationLagMs: float64(p.ReplicationLag.Milliseconds()),
				PingMs:           pingMs,
			}
		}
		msg := &pb.PeerHealthReply{
			NodeId: r.NodeID,
			Ntp: &pb.ClockHealth{
				NtpOffsetMs: r.NTP.NTPOffsetMs,
				NtpHealthy:  r.NTP.NTPHealthy,
				NtpError:    r.NTP.NTPError,
			},
			Peers: peers,
		}
		if r.MachineAddr != "" || r.MachineID != "" || r.Error != "" {
			msg.Metadata = &pb.Metadata{
				MachineAddr: r.MachineAddr,
				MachineId:   r.MachineID,
				Error:       r.Error,
			}
		}
		messages[i] = msg
	}
	return &pb.GetPeerHealthResponse{Messages: messages}
}

// --- Error mapping ---

func toGRPCError(err error) error {
	if err == nil {
		return nil
	}

	if errors.Is(err, os.ErrNotExist) || errors.Is(err, mesh.ErrNotInitialized) {
		return status.Error(codes.NotFound, err.Error())
	}
	if errors.Is(err, mesh.ErrConflict) {
		return status.Error(codes.FailedPrecondition, err.Error())
	}
	var valErr *mesh.ValidationError
	if errors.As(err, &valErr) {
		return status.Error(codes.InvalidArgument, err.Error())
	}

	// Fallback to string matching for errors not yet converted to typed sentinels.
	msg := err.Error()

	if strings.Contains(msg, "is not initialized") {
		return status.Error(codes.NotFound, msg)
	}
	if strings.Contains(msg, "is required") ||
		strings.Contains(msg, "must be") ||
		strings.Contains(msg, "parse ") {
		return status.Error(codes.InvalidArgument, msg)
	}
	if strings.Contains(msg, "connect to docker") ||
		strings.Contains(msg, "docker daemon") {
		return status.Error(codes.Unavailable, msg)
	}

	return status.Error(codes.Internal, msg)
}

// --- Utilities ---

func listenUnix(socketPath string) (net.Listener, error) {
	if err := os.MkdirAll(filepath.Dir(socketPath), 0o755); err != nil {
		return nil, fmt.Errorf("create socket directory: %w", err)
	}
	if err := os.Remove(socketPath); err != nil && !errors.Is(err, os.ErrNotExist) {
		return nil, fmt.Errorf("remove stale socket: %w", err)
	}

	ln, err := net.Listen("unix", socketPath)
	if err != nil {
		return nil, fmt.Errorf("listen unix: %w", err)
	}
	if err := os.Chmod(socketPath, 0o660); err != nil {
		_ = ln.Close() // best-effort cleanup
		return nil, fmt.Errorf("set socket permissions: %w", err)
	}
	if err := ensureSocketGroup(socketPath); err != nil {
		_ = ln.Close() // best-effort cleanup
		return nil, err
	}
	return ln, nil
}

func internalSocketPath(externalPath string) string {
	dir := filepath.Dir(externalPath)
	base := filepath.Base(externalPath)
	ext := filepath.Ext(base)
	name := strings.TrimSuffix(base, ext)
	return filepath.Join(dir, name+"-internal"+ext)
}

func ensureSocketGroup(socketPath string) error {
	switch runtime.GOOS {
	case "darwin":
		if err := os.Chmod(socketPath, 0o666); err != nil {
			if errors.Is(err, os.ErrPermission) {
				return nil
			}
			return fmt.Errorf("set daemon socket permissions: %w", err)
		}
		return nil
	case "linux":
		group, err := user.LookupGroup("ployz")
		if err != nil {
			return nil
		}
		gid, err := strconv.Atoi(group.Gid)
		if err != nil {
			return nil
		}
		if err := os.Chown(socketPath, -1, gid); err != nil {
			if errors.Is(err, os.ErrPermission) {
				return nil
			}
			return fmt.Errorf("set daemon socket group: %w", err)
		}
		return nil
	default:
		return nil
	}
}

// proxyMapper adapts the supervisor manager to the proxy.MachineMapper interface.
type proxyMapper struct {
	manager *supervisor.Manager
}

func (m proxyMapper) ListMachines(ctx context.Context, network string) ([]proxymod.MachineInfo, error) {
	addrs, err := m.manager.ListMachineAddrs(ctx, network)
	if err != nil {
		return nil, err
	}
	out := make([]proxymod.MachineInfo, len(addrs))
	for i, a := range addrs {
		out[i] = proxymod.MachineInfo{
			ID:           a.ID,
			ManagementIP: a.ManagementIP,
			OverlayIP:    a.OverlayIP,
		}
	}
	return out, nil
}
