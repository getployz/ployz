package daemon

import (
	"context"
	"fmt"
	"net"
	"os"

	"ployz"
	"ployz/daemon/pb"

	"google.golang.org/grpc"
)

// Machine is the interface the API server needs from the machine.
type Machine interface {
	Status() ployz.Machine
	InitNetwork(ctx context.Context, name string) error
}

type Server struct {
	pb.UnimplementedDaemonServer
	machine Machine
}

func NewServer(m Machine) *Server {
	return &Server{machine: m}
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
	if err := s.machine.InitNetwork(ctx, req.GetName()); err != nil {
		return nil, err
	}
	return &pb.InitNetworkResponse{}, nil
}

// ListenAndServe starts the gRPC server on a unix socket and blocks until
// ctx is cancelled.
func (s *Server) ListenAndServe(ctx context.Context, socketPath string) error {
	// Remove stale socket.
	_ = os.Remove(socketPath)

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
	_ = os.Remove(socketPath)
	return nil
}
