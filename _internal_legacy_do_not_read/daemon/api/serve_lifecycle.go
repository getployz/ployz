package api

import (
	"context"
	"fmt"
	"log/slog"
	"os"

	pb "ployz/internal/daemon/pb"
	proxymod "ployz/internal/daemon/proxy"
	"ployz/pkg/sdk/defaults"

	grpcproxy "github.com/siderolabs/grpc-proxy/proxy"
	"go.opentelemetry.io/contrib/instrumentation/google.golang.org/grpc/otelgrpc"
	"google.golang.org/grpc"
)

// ListenAndServe starts the daemon with a proxy layer.
func (s *Server) ListenAndServe(ctx context.Context, socketPath string) error {
	log := slog.With("component", "daemon-server", "socket", socketPath)
	internalSockPath := internalSocketPath(socketPath)
	phase := ServeStartingInternal

	// Always start the direct gRPC server on the internal socket.
	directSrv := grpc.NewServer(grpc.StatsHandler(otelgrpc.NewServerHandler()))
	pb.RegisterDaemonServer(directSrv, s)

	directLn, err := listenUnix(internalSockPath)
	if err != nil {
		return fmt.Errorf("listen internal socket: %w", err)
	}
	phase = phase.Transition(ServeStartingProxy)
	log.Debug("internal listener started", "socket", internalSockPath)
	serveErr := make(chan error, serverGoroutineCount)
	go func() { serveErr <- directSrv.Serve(directLn) }()

	// Create the proxy director.
	mapper := proxyMapper{manager: s.manager}
	remotePort := uint16(defaults.DaemonAPIPort("default"))
	director := proxymod.NewDirector(internalSockPath, remotePort, mapper)

	proxySrv := grpc.NewServer(
		grpc.StatsHandler(otelgrpc.NewServerHandler()),
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
	phase = phase.Transition(ServeWaitingForIdentity)
	log.Debug("proxy listener started", "socket", socketPath)
	go func() { serveErr <- proxySrv.Serve(proxyLn) }()

	// Start a goroutine that watches for the identity to become available,
	// then starts the TCP listener and updates the director.
	go s.watchIdentityAndServeTCP(ctx, director, serveErr)
	phase = phase.Transition(ServeServing)

	var retErr error
	select {
	case <-ctx.Done():
		log.Info("shutting down listeners")
	case retErr = <-serveErr:
		log.Error("listener exited", "err", retErr)
	}
	phase = phase.Transition(ServeShuttingDown)

	proxySrv.GracefulStop()
	directSrv.GracefulStop()
	director.Close()
	_ = os.Remove(socketPath)       // best-effort cleanup
	_ = os.Remove(internalSockPath) // best-effort cleanup
	log.Debug("server lifecycle phase", "phase", phase.String())
	return retErr
}
