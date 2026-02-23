package server

import (
	"time"

	pb "ployz/internal/daemon/pb"
	"ployz/internal/daemon/supervisor"
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
