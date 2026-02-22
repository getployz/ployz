package proxy

import (
	"context"
	"fmt"
	"log/slog"
	"net/netip"
	"sync"
	"time"

	"github.com/siderolabs/grpc-proxy/proxy"
	"google.golang.org/grpc"
	"google.golang.org/grpc/backoff"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/metadata"
)

// RemoteBackend proxies to a remote gRPC server over TCP (WireGuard).
type RemoteBackend struct {
	One2ManyResponder
	target string

	mu   sync.RWMutex
	conn *grpc.ClientConn
}

var _ proxy.Backend = (*RemoteBackend)(nil)

// NewRemoteBackend creates a RemoteBackend for the given management IP and port.
func NewRemoteBackend(addr string, port uint16, id string) (*RemoteBackend, error) {
	ip, err := netip.ParseAddr(addr)
	if err != nil {
		return nil, fmt.Errorf("invalid management IP: %s", addr)
	}

	return &RemoteBackend{
		One2ManyResponder: One2ManyResponder{
			machineAddr: addr,
			machineID:   id,
		},
		target: netip.AddrPortFrom(ip, port).String(),
	}, nil
}

func (b *RemoteBackend) String() string {
	return b.machineAddr
}

// GetConnection returns a gRPC connection to the remote server.
func (b *RemoteBackend) GetConnection(ctx context.Context, _ string) (context.Context, *grpc.ClientConn, error) {
	md, _ := metadata.FromIncomingContext(ctx)
	if authority := md[":authority"]; len(authority) > 0 {
		md.Set("proxy-authority", authority...)
	} else {
		md.Set("proxy-authority", "unknown")
	}
	delete(md, ":authority")
	delete(md, "machines")
	delete(md, "proxy-network")

	outCtx := metadata.NewOutgoingContext(ctx, md)

	b.mu.RLock()
	if b.conn != nil {
		defer b.mu.RUnlock()
		return outCtx, b.conn, nil
	}
	b.mu.RUnlock()

	b.mu.Lock()
	defer b.mu.Unlock()

	backoffConfig := backoff.DefaultConfig
	backoffConfig.MaxDelay = 15 * time.Second

	var err error
	b.conn, err = grpc.NewClient(
		b.target,
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithConnectParams(grpc.ConnectParams{
			Backoff:           backoffConfig,
			MinConnectTimeout: 10 * time.Second,
		}),
		grpc.WithDefaultCallOptions(
			grpc.ForceCodecV2(proxy.Codec()),
		),
	)
	if err == nil {
		slog.Debug("proxy remote backend connected", "component", "proxy-remote", "target", b.target, "machine_id", b.machineID)
	}

	return outCtx, b.conn, err
}

// Close closes the upstream gRPC connection.
func (b *RemoteBackend) Close() {
	b.mu.Lock()
	defer b.mu.Unlock()

	if b.conn != nil {
		b.conn.Close()
		b.conn = nil
		slog.Debug("proxy remote backend closed", "component", "proxy-remote", "target", b.target, "machine_id", b.machineID)
	}
}
