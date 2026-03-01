package daemon

import (
	"context"
	"fmt"
	"net"
	"os"

	"ployz"
	"ployz/daemon/pb"
	"ployz/machine"

	"google.golang.org/grpc"
)

// Machine is the interface the API server needs from the machine.
type Machine interface {
	Status() ployz.Machine
	HasMeshAttached() bool
	InitNetwork(ctx context.Context, name string, ns machine.NetworkStack) error
}

type Server struct {
	pb.UnimplementedDaemonServer
	machine   Machine
	buildMesh machine.MeshBuilder
}

func NewServer(m Machine, buildMesh machine.MeshBuilder) *Server {
	return &Server{machine: m, buildMesh: buildMesh}
}

func (s *Server) GetStatus(_ context.Context, _ *pb.GetStatusRequest) (*pb.GetStatusResponse, error) {
	st := s.machine.Status()
	return &pb.GetStatusResponse{
		Name:         st.Name,
		PublicKey:    st.PublicKey,
		NetworkPhase: st.NetworkPhase,
		Version:      st.Version,
	}, nil
}

func (s *Server) InitNetwork(ctx context.Context, req *pb.InitNetworkRequest) (*pb.InitNetworkResponse, error) {
	// Preflight — avoid building if obviously not needed. Not authoritative; machine rechecks.
	if s.machine.HasMeshAttached() {
		return nil, fmt.Errorf("network already running")
	}
	if s.buildMesh == nil {
		return nil, fmt.Errorf("networking not supported on this platform")
	}
	ns, err := s.buildMesh(ctx)
	if err != nil {
		return nil, fmt.Errorf("build mesh: %w", err)
	}
	if err := s.machine.InitNetwork(ctx, req.GetName(), ns); err != nil {
		return nil, err
	}
	return &pb.InitNetworkResponse{}, nil
}

// ListenAndServe starts the gRPC server on a unix socket and blocks until
// ctx is cancelled.
func (s *Server) ListenAndServe(ctx context.Context, socketPath string) error {
	// Remove stale socket from a previous run (may not exist).
	_ = os.Remove(socketPath)
	defer func() { _ = os.Remove(socketPath) }()

	ln, err := net.Listen("unix", socketPath)
	if err != nil {
		return fmt.Errorf("listen unix %s: %w", socketPath, err)
	}

	srv := grpc.NewServer()
	pb.RegisterDaemonServer(srv, s)

	// Shut down when ctx is cancelled.
	go func() {
		<-ctx.Done()
		srv.GracefulStop()
	}()

	if err := srv.Serve(ln); err != nil {
		return fmt.Errorf("serve: %w", err)
	}
	return nil
}
