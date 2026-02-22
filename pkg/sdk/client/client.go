package client

import (
	"context"
	"errors"
	"fmt"
	"net"
	"os"
	"runtime"
	"strings"
	"time"

	pb "ployz/internal/daemon/pb"
	"ployz/pkg/sdk/types"

	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
)

const envSocket = "PLOYZD_SOCKET"

func DefaultSocketPath() string {
	if fromEnv := strings.TrimSpace(os.Getenv(envSocket)); fromEnv != "" {
		return fromEnv
	}
	if runtime.GOOS == "darwin" {
		return "/tmp/ployzd.sock"
	}
	return "/var/run/ployzd.sock"
}

type API interface {
	ApplyNetworkSpec(ctx context.Context, spec types.NetworkSpec) (types.ApplyResult, error)
	DisableNetwork(ctx context.Context, network string, purge bool) error
	GetStatus(ctx context.Context, network string) (types.NetworkStatus, error)
	GetIdentity(ctx context.Context, network string) (types.Identity, error)
	ListMachines(ctx context.Context, network string) ([]types.MachineEntry, error)
	UpsertMachine(ctx context.Context, network string, m types.MachineEntry) error
	RemoveMachine(ctx context.Context, network string, idOrEndpoint string) error
	TriggerReconcile(ctx context.Context, network string) error
	GetPeerHealth(ctx context.Context, network string) ([]types.PeerHealthResponse, error)
}

type Client struct {
	conn   *grpc.ClientConn
	daemon pb.DaemonClient
}

func NewUnix(socketPath string) (*Client, error) {
	conn, err := grpc.NewClient(
		"unix://"+socketPath,
		grpc.WithTransportCredentials(insecure.NewCredentials()),
	)
	if err != nil {
		return nil, fmt.Errorf("grpc dial unix socket: %w", err)
	}
	return &Client{conn: conn, daemon: pb.NewDaemonClient(conn)}, nil
}

func NewWithDialer(dialer func(ctx context.Context, addr string) (net.Conn, error)) (*Client, error) {
	conn, err := grpc.NewClient(
		"passthrough:///ployzd",
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithContextDialer(dialer),
	)
	if err != nil {
		return nil, fmt.Errorf("grpc dial with custom dialer: %w", err)
	}
	return &Client{conn: conn, daemon: pb.NewDaemonClient(conn)}, nil
}

func (c *Client) Close() error {
	return c.conn.Close()
}

func (c *Client) ApplyNetworkSpec(ctx context.Context, spec types.NetworkSpec) (types.ApplyResult, error) {
	resp, err := c.daemon.ApplyNetworkSpec(ctx, &pb.ApplyNetworkSpecRequest{Spec: specToProto(spec)})
	if err != nil {
		return types.ApplyResult{}, grpcErr(err)
	}
	return applyResultFromProto(resp), nil
}

func (c *Client) DisableNetwork(ctx context.Context, network string, purge bool) error {
	_, err := c.daemon.DisableNetwork(ctx, &pb.DisableNetworkRequest{Network: network, Purge: purge})
	return grpcErr(err)
}

func (c *Client) GetStatus(ctx context.Context, network string) (types.NetworkStatus, error) {
	resp, err := c.daemon.GetStatus(ctx, &pb.GetStatusRequest{Network: network})
	if err != nil {
		return types.NetworkStatus{}, grpcErr(err)
	}
	st := types.NetworkStatus{
		Configured:    resp.Configured,
		Running:       resp.Running,
		WireGuard:     resp.Wireguard,
		Corrosion:     resp.Corrosion,
		DockerNet:     resp.Docker,
		StatePath:     resp.StatePath,
		WorkerRunning: resp.WorkerRunning,
	}
	if resp.ClockHealth != nil {
		st.ClockHealth = types.ClockHealth{
			NTPOffsetMs: resp.ClockHealth.NtpOffsetMs,
			NTPHealthy:  resp.ClockHealth.NtpHealthy,
			NTPError:    resp.ClockHealth.NtpError,
		}
	}
	return st, nil
}

func (c *Client) GetIdentity(ctx context.Context, network string) (types.Identity, error) {
	resp, err := c.daemon.GetIdentity(ctx, &pb.GetIdentityRequest{Network: network})
	if err != nil {
		return types.Identity{}, grpcErr(err)
	}
	return types.Identity{
		ID:                resp.Id,
		PublicKey:         resp.PublicKey,
		Subnet:            resp.Subnet,
		ManagementIP:      resp.ManagementIp,
		AdvertiseEndpoint: resp.AdvertiseEndpoint,
		NetworkCIDR:       resp.NetworkCidr,
		WGInterface:       resp.WgInterface,
		WGPort:            int(resp.WgPort),
		HelperName:        resp.HelperName,
		CorrosionGossip:   int(resp.CorrosionGossipPort),
		CorrosionMemberID: resp.CorrosionMemberId,
		CorrosionAPIToken: resp.CorrosionApiToken,
		Running:           resp.Running,
	}, nil
}

func (c *Client) ListMachines(ctx context.Context, network string) ([]types.MachineEntry, error) {
	resp, err := c.daemon.ListMachines(ctx, &pb.ListMachinesRequest{Network: network})
	if err != nil {
		return nil, grpcErr(err)
	}
	out := make([]types.MachineEntry, len(resp.Machines))
	for i, m := range resp.Machines {
		out[i] = machineFromProto(m)
	}
	return out, nil
}

func (c *Client) UpsertMachine(ctx context.Context, network string, m types.MachineEntry) error {
	_, err := c.daemon.UpsertMachine(ctx, &pb.UpsertMachineRequest{
		Network: network,
		Machine: machineToProto(m),
	})
	return grpcErr(err)
}

func (c *Client) RemoveMachine(ctx context.Context, network string, idOrEndpoint string) error {
	_, err := c.daemon.RemoveMachine(ctx, &pb.RemoveMachineRequest{
		Network:      network,
		IdOrEndpoint: idOrEndpoint,
	})
	return grpcErr(err)
}

func (c *Client) TriggerReconcile(ctx context.Context, network string) error {
	_, err := c.daemon.TriggerReconcile(ctx, &pb.TriggerReconcileRequest{Network: network})
	return grpcErr(err)
}

func (c *Client) GetPeerHealth(ctx context.Context, network string) ([]types.PeerHealthResponse, error) {
	resp, err := c.daemon.GetPeerHealth(ctx, &pb.GetPeerHealthRequest{Network: network})
	if err != nil {
		return nil, grpcErr(err)
	}
	out := make([]types.PeerHealthResponse, 0, len(resp.Messages))
	for _, msg := range resp.Messages {
		r := types.PeerHealthResponse{
			NodeID: msg.NodeId,
		}
		if msg.Metadata != nil {
			r.MachineAddr = msg.Metadata.MachineAddr
			r.MachineID = msg.Metadata.MachineId
			r.Error = msg.Metadata.Error
		}
		if msg.Ntp != nil {
			r.NTP = types.ClockHealth{
				NTPOffsetMs: msg.Ntp.NtpOffsetMs,
				NTPHealthy:  msg.Ntp.NtpHealthy,
				NTPError:    msg.Ntp.NtpError,
			}
		}
		for _, p := range msg.Peers {
			pingRTT := time.Duration(p.PingMs * float64(time.Millisecond))
			if p.PingMs < 0 {
				pingRTT = -1
			}
			r.Peers = append(r.Peers, types.PeerLag{
				NodeID:         p.NodeId,
				Freshness:      time.Duration(p.FreshnessMs) * time.Millisecond,
				Stale:          p.Stale,
				ReplicationLag: time.Duration(p.ReplicationLagMs) * time.Millisecond,
				PingRTT:        pingRTT,
			})
		}
		out = append(out, r)
	}
	return out, nil
}

// ProxyMachinesContext returns a context that routes gRPC requests through
// the proxy to the specified machines. If nodeIDs is nil, all machines are targeted.
func ProxyMachinesContext(ctx context.Context, network string, nodeIDs []string) context.Context {
	md := metadata.New(nil)
	md.Set("proxy-network", network)
	if len(nodeIDs) == 0 {
		md.Append("machines", "*")
	} else {
		for _, id := range nodeIDs {
			md.Append("machines", id)
		}
	}
	return metadata.NewOutgoingContext(ctx, md)
}

func specToProto(s types.NetworkSpec) *pb.NetworkSpec {
	return &pb.NetworkSpec{
		Network:           s.Network,
		DataRoot:          s.DataRoot,
		NetworkCidr:       s.NetworkCIDR,
		Subnet:            s.Subnet,
		ManagementIp:      s.ManagementIP,
		AdvertiseEndpoint: s.AdvertiseEndpoint,
		WgPort:            int32(s.WGPort),
		CorrosionMemberId: s.CorrosionMemberID,
		CorrosionApiToken: s.CorrosionAPIToken,
		Bootstrap:         s.Bootstrap,
		HelperImage:       s.HelperImage,
	}
}

func applyResultFromProto(r *pb.ApplyResult) types.ApplyResult {
	return types.ApplyResult{
		Network:            r.Network,
		NetworkCIDR:        r.NetworkCidr,
		Subnet:             r.Subnet,
		ManagementIP:       r.ManagementIp,
		WGInterface:        r.WgInterface,
		WGPort:             int(r.WgPort),
		AdvertiseEndpoint:  r.AdvertiseEndpoint,
		CorrosionName:      r.CorrosionName,
		CorrosionAPIAddr:   r.CorrosionApiAddr,
		CorrosionGossipAP:  r.CorrosionGossipAddr,
		DockerNetwork:      r.DockerNetwork,
		ConvergenceRunning: r.ConvergenceRunning,
	}
}

func machineToProto(m types.MachineEntry) *pb.MachineEntry {
	return &pb.MachineEntry{
		Id:              m.ID,
		PublicKey:       m.PublicKey,
		Subnet:          m.Subnet,
		ManagementIp:    m.ManagementIP,
		Endpoint:        m.Endpoint,
		LastUpdated:     m.LastUpdated,
		Version:         m.Version,
		ExpectedVersion: m.ExpectedVersion,
	}
}

func machineFromProto(p *pb.MachineEntry) types.MachineEntry {
	return types.MachineEntry{
		ID:              p.Id,
		PublicKey:       p.PublicKey,
		Subnet:          p.Subnet,
		ManagementIP:    p.ManagementIp,
		Endpoint:        p.Endpoint,
		LastUpdated:     p.LastUpdated,
		Version:         p.Version,
		ExpectedVersion: p.ExpectedVersion,
		Freshness:       time.Duration(p.FreshnessMs) * time.Millisecond,
		Stale:           p.Stale,
		ReplicationLag:  time.Duration(p.ReplicationLagMs) * time.Millisecond,
	}
}

var (
	ErrConflict    = errors.New("version conflict")
	ErrNotFound    = errors.New("not found")
	ErrValidation  = errors.New("validation error")
	ErrUnavailable = errors.New("unavailable")
)

func grpcErr(err error) error {
	if err == nil {
		return nil
	}
	st, ok := status.FromError(err)
	if !ok {
		return err
	}
	switch st.Code() {
	case codes.NotFound:
		return fmt.Errorf("%w: %s", ErrNotFound, st.Message())
	case codes.InvalidArgument:
		return fmt.Errorf("%w: %s", ErrValidation, st.Message())
	case codes.Unavailable:
		return fmt.Errorf("%w: %s", ErrUnavailable, st.Message())
	case codes.FailedPrecondition:
		return fmt.Errorf("%w: %s", ErrConflict, st.Message())
	default:
		return errors.New(st.Message())
	}
}
