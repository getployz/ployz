package api

import (
	"time"

	"ployz/machine"
	pb "ployz/internal/daemon/pb"
)

const (
	// identityPollInterval is 2s: balances responsiveness with CPU cost when waiting for first network setup.
	identityPollInterval = 2 * time.Second
	// serverGoroutineCount is 3: direct gRPC server + proxy server + TCP identity watcher.
	serverGoroutineCount = 3
)

type Server struct {
	pb.UnimplementedDaemonServer
	machine *machine.Machine
}

func New(m *machine.Machine) *Server {
	return &Server{machine: m}
}
