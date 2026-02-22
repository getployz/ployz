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

	grpcproxy "github.com/siderolabs/grpc-proxy/proxy"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

type Server struct {
	pb.UnimplementedDaemonServer
	mgr *supervisor.Manager
}

func New(mgr *supervisor.Manager) *Server {
	return &Server{mgr: mgr}
}

// ListenAndServe starts the daemon with a proxy layer.
//
// Socket layout:
//   - socketPath — proxy gRPC (CLI connects here, routes via director)
//   - internalSockPath — direct daemon gRPC (local backend target)
//   - [managementIP]:DaemonAPIPort — TCP for remote proxy connections
//
// If managementIP is empty, no TCP listener or proxy is started and the daemon
// listens on socketPath directly (single-socket fallback for non-initialized nodes).
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
	serveErr := make(chan error, 3)
	go func() { serveErr <- directSrv.Serve(directLn) }()

	// Create the proxy director.
	mapper := proxyMapper{mgr: s.mgr}
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
		_ = os.Remove(internalSockPath)
		return fmt.Errorf("listen proxy socket: %w", err)
	}
	log.Debug("proxy listener started", "socket", socketPath)
	go func() { serveErr <- proxySrv.Serve(proxyLn) }()

	// Start a goroutine that watches for the identity to become available,
	// then starts the TCP listener and updates the director.
	go s.watchIdentityAndServeTCP(ctx, director, serveErr)

	select {
	case <-ctx.Done():
		log.Info("shutting down listeners")
		proxySrv.GracefulStop()
		directSrv.GracefulStop()
		director.Close()
		_ = os.Remove(socketPath)
		_ = os.Remove(internalSockPath)
		return nil
	case err := <-serveErr:
		log.Error("listener exited", "err", err)
		proxySrv.GracefulStop()
		directSrv.GracefulStop()
		director.Close()
		_ = os.Remove(socketPath)
		_ = os.Remove(internalSockPath)
		return err
	}
}

// watchIdentityAndServeTCP polls for the network identity and starts the TCP listener
// once the management IP is known. It also updates the director with local machine info.
func (s *Server) watchIdentityAndServeTCP(ctx context.Context, director *proxymod.Director, serveErr chan<- error) {
	ticker := time.NewTicker(2 * time.Second)
	defer ticker.Stop()

	// Poll until identity is available or context is cancelled.
	var identity *pb.Identity
	for {
		id, err := s.mgr.GetIdentity(ctx, "default")
		if err == nil && strings.TrimSpace(id.ManagementIp) != "" {
			identity = id
			break
		}
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
	}

	mgmtIP := strings.TrimSpace(identity.ManagementIp)
	nodeID := strings.TrimSpace(identity.Id)

	director.UpdateLocalMachine(nodeID, mgmtIP)
	slog.Debug("proxy: local identity resolved", "node_id", nodeID[:min(8, len(nodeID))]+"…", "mgmt_ip", mgmtIP)

	// Determine remote port (same port range used by the director for remotes).
	network := "default"
	port := defaults.DaemonAPIPort(network)
	portStr := strconv.Itoa(port)

	// Collect listen addresses: management IPv6 + overlay IPv4.
	listenAddrs := []string{net.JoinHostPort(mgmtIP, portStr)}
	if prefix, err := netip.ParsePrefix(strings.TrimSpace(identity.Subnet)); err == nil {
		overlayIP := prefix.Masked().Addr().Next()
		listenAddrs = append(listenAddrs, net.JoinHostPort(overlayIP.String(), portStr))
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
			// Non-fatal: the interface might not be up yet (common on macOS where
			// WireGuard runs inside Docker and the management IPv6 isn't local).
			slog.Debug("proxy: TCP listen failed (non-fatal)", "addr", addr, "err", err)
			continue
		}
		slog.Debug("proxy: TCP listener started", "addr", addr)
		go func() { serveErr <- tcpSrv.Serve(tcpLn) }()
	}
}

func (s *Server) ApplyNetworkSpec(ctx context.Context, req *pb.ApplyNetworkSpecRequest) (*pb.ApplyResult, error) {
	if req.Spec == nil {
		return nil, status.Error(codes.InvalidArgument, "spec is required")
	}
	out, err := s.mgr.ApplyNetworkSpec(ctx, req.Spec)
	if err != nil {
		return nil, toGRPCError(err)
	}
	return out, nil
}

func (s *Server) DisableNetwork(ctx context.Context, req *pb.DisableNetworkRequest) (*pb.DisableNetworkResponse, error) {
	if err := s.mgr.DisableNetwork(ctx, req.Network, req.Purge); err != nil {
		return nil, toGRPCError(err)
	}
	return &pb.DisableNetworkResponse{}, nil
}

func (s *Server) GetStatus(ctx context.Context, req *pb.GetStatusRequest) (*pb.NetworkStatus, error) {
	st, err := s.mgr.GetStatus(ctx, req.Network)
	if err != nil {
		return nil, toGRPCError(err)
	}
	return st, nil
}

func (s *Server) GetIdentity(ctx context.Context, req *pb.GetIdentityRequest) (*pb.Identity, error) {
	id, err := s.mgr.GetIdentity(ctx, req.Network)
	if err != nil {
		return nil, toGRPCError(err)
	}
	return id, nil
}

func (s *Server) ListMachines(ctx context.Context, req *pb.ListMachinesRequest) (*pb.ListMachinesResponse, error) {
	machines, err := s.mgr.ListMachines(ctx, req.Network)
	if err != nil {
		return nil, toGRPCError(err)
	}
	return &pb.ListMachinesResponse{Machines: machines}, nil
}

func (s *Server) UpsertMachine(ctx context.Context, req *pb.UpsertMachineRequest) (*pb.UpsertMachineResponse, error) {
	if req.Machine == nil {
		return nil, status.Error(codes.InvalidArgument, "machine is required")
	}
	if err := s.mgr.UpsertMachine(ctx, req.Network, req.Machine); err != nil {
		return nil, toGRPCError(err)
	}
	return &pb.UpsertMachineResponse{}, nil
}

func (s *Server) RemoveMachine(ctx context.Context, req *pb.RemoveMachineRequest) (*pb.RemoveMachineResponse, error) {
	if err := s.mgr.RemoveMachine(ctx, req.Network, req.IdOrEndpoint); err != nil {
		return nil, toGRPCError(err)
	}
	return &pb.RemoveMachineResponse{}, nil
}

func (s *Server) TriggerReconcile(ctx context.Context, req *pb.TriggerReconcileRequest) (*pb.TriggerReconcileResponse, error) {
	if err := s.mgr.TriggerReconcile(ctx, req.Network); err != nil {
		return nil, toGRPCError(err)
	}
	return &pb.TriggerReconcileResponse{}, nil
}

func (s *Server) GetPeerHealth(ctx context.Context, req *pb.GetPeerHealthRequest) (*pb.GetPeerHealthResponse, error) {
	resp, err := s.mgr.GetPeerHealth(ctx, req.Network)
	if err != nil {
		return nil, toGRPCError(err)
	}
	return resp, nil
}

func toGRPCError(err error) error {
	if err == nil {
		return nil
	}

	if errors.Is(err, os.ErrNotExist) {
		return status.Error(codes.NotFound, err.Error())
	}
	if errors.Is(err, mesh.ErrConflict) {
		return status.Error(codes.FailedPrecondition, err.Error())
	}

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
		_ = ln.Close()
		return nil, fmt.Errorf("set socket permissions: %w", err)
	}
	if err := ensureSocketGroup(socketPath); err != nil {
		_ = ln.Close()
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
		// Daemon runs as root; make socket world-accessible so non-root CLI can connect.
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
	mgr *supervisor.Manager
}

func (m proxyMapper) ListMachines(ctx context.Context, network string) ([]proxymod.MachineInfo, error) {
	addrs, err := m.mgr.ListMachineAddrs(ctx, network)
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
