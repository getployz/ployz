package server

import (
	"context"
	"log/slog"
	"net"
	"net/netip"
	"strconv"
	"strings"
	"time"

	proxymod "ployz/internal/daemon/proxy"
	"ployz/internal/mesh"
	"ployz/pkg/sdk/defaults"
	"ployz/pkg/sdk/types"

	grpcproxy "github.com/siderolabs/grpc-proxy/proxy"
	"google.golang.org/grpc"
)

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
	slog.Debug("proxy: local identity resolved", "node_id", nodeID[:min(8, len(nodeID))]+"...", "mgmt_ip", mgmtIP)

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
