package proxy

import (
	"context"
	"log/slog"
	"sync"

	"github.com/siderolabs/grpc-proxy/proxy"
	"go.opentelemetry.io/contrib/instrumentation/google.golang.org/grpc/otelgrpc"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/metadata"
)

// LocalBackend proxies to a local gRPC server listening on a Unix socket.
type LocalBackend struct {
	One2ManyResponder
	sockPath string

	mu   sync.RWMutex
	conn *grpc.ClientConn
}

var _ proxy.Backend = (*LocalBackend)(nil)

// NewLocalBackend returns a new LocalBackend for the given Unix socket path.
func NewLocalBackend(sockPath, addr, id string) *LocalBackend {
	return &LocalBackend{
		One2ManyResponder: One2ManyResponder{
			machineAddr: addr,
			machineID:   id,
		},
		sockPath: sockPath,
	}
}

func (b *LocalBackend) String() string {
	return b.machineAddr
}

// GetConnection returns a gRPC connection to the local server.
func (b *LocalBackend) GetConnection(ctx context.Context, _ string) (context.Context, *grpc.ClientConn, error) {
	md, _ := metadata.FromIncomingContext(ctx)
	outCtx := metadata.NewOutgoingContext(ctx, md)

	b.mu.RLock()
	if b.conn != nil {
		defer b.mu.RUnlock()
		return outCtx, b.conn, nil
	}
	b.mu.RUnlock()

	b.mu.Lock()
	defer b.mu.Unlock()

	var err error
	b.conn, err = grpc.NewClient(
		"unix://"+b.sockPath,
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithStatsHandler(otelgrpc.NewClientHandler()),
		grpc.WithDefaultCallOptions(
			grpc.ForceCodecV2(proxy.Codec()),
		),
	)
	if err == nil {
		slog.Debug("proxy local backend connected", "component", "proxy-local", "socket", b.sockPath)
	}

	return outCtx, b.conn, err
}

// Close closes the upstream gRPC connection.
func (b *LocalBackend) Close() {
	b.mu.Lock()
	defer b.mu.Unlock()

	if b.conn != nil {
		b.conn.Close()
		b.conn = nil
		slog.Debug("proxy local backend closed", "component", "proxy-local", "socket", b.sockPath)
	}
}
