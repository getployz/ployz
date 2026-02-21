package server

import (
	"context"
	"errors"
	"fmt"
	"net"
	"os"
	"os/user"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"

	"ployz/internal/coordination/registry"
	pb "ployz/internal/daemon/pb"
	"ployz/internal/daemon/supervisor"

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

func (s *Server) ListenAndServe(ctx context.Context, socketPath string) error {
	if err := os.MkdirAll(filepath.Dir(socketPath), 0o755); err != nil {
		return fmt.Errorf("create daemon socket directory: %w", err)
	}
	if err := os.Remove(socketPath); err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("remove stale daemon socket: %w", err)
	}

	ln, err := net.Listen("unix", socketPath)
	if err != nil {
		return fmt.Errorf("listen unix socket: %w", err)
	}
	if err := os.Chmod(socketPath, 0o660); err != nil {
		_ = ln.Close()
		return fmt.Errorf("set daemon socket permissions: %w", err)
	}
	if err := ensureSocketGroup(socketPath); err != nil {
		_ = ln.Close()
		return err
	}

	grpcSrv := grpc.NewServer()
	pb.RegisterDaemonServer(grpcSrv, s)

	serveErr := make(chan error, 1)
	go func() {
		serveErr <- grpcSrv.Serve(ln)
	}()

	select {
	case <-ctx.Done():
		grpcSrv.GracefulStop()
		_ = os.Remove(socketPath)
		return nil
	case err := <-serveErr:
		_ = os.Remove(socketPath)
		return err
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

func toGRPCError(err error) error {
	if err == nil {
		return nil
	}

	if errors.Is(err, os.ErrNotExist) {
		return status.Error(codes.NotFound, err.Error())
	}
	if errors.Is(err, registry.ErrConflict) {
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

func ensureSocketGroup(socketPath string) error {
	if runtime.GOOS != "linux" {
		return nil
	}
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
}
