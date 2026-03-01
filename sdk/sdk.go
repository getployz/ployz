// Package sdk provides a Go client for the ployz daemon.
// CLI commands and external tools use this to communicate
// with a local or remote daemon.
package sdk

import (
	"context"
	"fmt"
	"strings"

	"ployz"
	"ployz/daemon/pb"

	"google.golang.org/grpc"
)

// Client wraps a gRPC connection to a ployz daemon.
type Client struct {
	conn   *grpc.ClientConn
	daemon pb.DaemonClient
}

// Dial connects to a daemon. If target contains "@" it is treated as an
// SSH destination (e.g. "root@host"); otherwise it is a local unix socket path.
func Dial(ctx context.Context, target string, opts ...DialOption) (*Client, error) {
	var cfg dialConfig
	for _, o := range opts {
		o(&cfg)
	}

	var (
		conn *grpc.ClientConn
		err  error
	)

	if strings.Contains(target, "@") {
		conn, err = dialSSH(ctx, target, cfg)
	} else {
		conn, err = dialUnix(ctx, target)
	}
	if err != nil {
		return nil, err
	}

	return &Client{
		conn:   conn,
		daemon: pb.NewDaemonClient(conn),
	}, nil
}

// CreateNetwork initializes a new network on the daemon.
func (c *Client) CreateNetwork(ctx context.Context, name string) error {
	_, err := c.daemon.InitNetwork(ctx, &pb.InitNetworkRequest{Name: name})
	if err != nil {
		return fmt.Errorf("init network: %w", err)
	}
	return nil
}

// Status returns the daemon's machine status.
func (c *Client) Status(ctx context.Context) (*ployz.Machine, error) {
	resp, err := c.daemon.GetStatus(ctx, &pb.GetStatusRequest{})
	if err != nil {
		return nil, fmt.Errorf("get status: %w", err)
	}
	return &ployz.Machine{
		Name:         resp.GetName(),
		PublicKey:    resp.GetPublicKey(),
		NetworkPhase: resp.GetNetworkPhase(),
		Version:      resp.GetVersion(),
	}, nil
}

// Close releases the underlying connection.
func (c *Client) Close() error {
	return c.conn.Close()
}
