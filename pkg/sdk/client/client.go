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

	"ployz/internal/daemon/pb"
	"ployz/pkg/sdk/types"

	"go.opentelemetry.io/contrib/instrumentation/google.golang.org/grpc/otelgrpc"
	"google.golang.org/genproto/googleapis/rpc/errdetails"
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
	DisableNetwork(ctx context.Context, purge bool) error
	GetStatus(ctx context.Context) (types.NetworkStatus, error)
	GetIdentity(ctx context.Context) (types.Identity, error)
	ListMachines(ctx context.Context) ([]types.MachineEntry, error)
	UpsertMachine(ctx context.Context, m types.MachineEntry) error
	RemoveMachine(ctx context.Context, idOrEndpoint string) error
	GetPeerHealth(ctx context.Context) ([]types.PeerHealthResponse, error)

	PlanDeploy(ctx context.Context, namespace string, composeSpec []byte) (types.DeployPlan, error)
	ApplyDeploy(ctx context.Context, namespace string, composeSpec []byte, events chan<- types.DeployProgressEvent) (types.DeployResult, error)
	ListDeployments(ctx context.Context, namespace string) ([]types.DeploymentEntry, error)
	RemoveNamespace(ctx context.Context, namespace string) error
	ReadContainerState(ctx context.Context, namespace string) ([]types.ContainerState, error)
}

type Client struct {
	conn   *grpc.ClientConn
	daemon pb.DaemonClient
}

func NewUnix(socketPath string) (*Client, error) {
	conn, err := grpc.NewClient(
		"unix://"+socketPath,
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithStatsHandler(otelgrpc.NewClientHandler()),
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
		grpc.WithStatsHandler(otelgrpc.NewClientHandler()),
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

func (c *Client) DisableNetwork(ctx context.Context, purge bool) error {
	_, err := c.daemon.DisableNetwork(ctx, &pb.DisableNetworkRequest{Purge: purge})
	return grpcErr(err)
}

func (c *Client) GetStatus(ctx context.Context) (types.NetworkStatus, error) {
	resp, err := c.daemon.GetStatus(ctx, &pb.GetStatusRequest{})
	if err != nil {
		return types.NetworkStatus{}, grpcErr(err)
	}
	st := types.NetworkStatus{
		Configured:        resp.Configured,
		Running:           resp.Running,
		WireGuard:         resp.Wireguard,
		Corrosion:         resp.Corrosion,
		DockerNet:         resp.Docker,
		StatePath:         resp.StatePath,
		SupervisorRunning: resp.SupervisorRunning,
		NetworkPhase:      resp.NetworkPhase,
		SupervisorPhase:   resp.SupervisorPhase,
		SupervisorError:   resp.SupervisorError,
		ClockPhase:        resp.ClockPhase,
		DockerRequired:    resp.DockerRequired,
		RuntimeTree:       stateNodeFromProto(resp.RuntimeTree),
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

func stateNodeFromProto(node *pb.StateNode) types.StateNode {
	if node == nil {
		return types.StateNode{}
	}
	out := types.StateNode{
		Component:     node.Component,
		Phase:         node.Phase,
		Required:      node.Required,
		Healthy:       node.Healthy,
		LastErrorCode: node.LastErrorCode,
		LastError:     node.LastError,
		Hint:          node.Hint,
	}
	if len(node.Children) == 0 {
		return out
	}
	out.Children = make([]types.StateNode, len(node.Children))
	for i, child := range node.Children {
		out.Children[i] = stateNodeFromProto(child)
	}
	return out
}

func (c *Client) GetIdentity(ctx context.Context) (types.Identity, error) {
	resp, err := c.daemon.GetIdentity(ctx, &pb.GetIdentityRequest{})
	if err != nil {
		return types.Identity{}, grpcErr(err)
	}
	return types.Identity{
		ID:                  resp.Id,
		PublicKey:           resp.PublicKey,
		Subnet:              resp.Subnet,
		ManagementIP:        resp.ManagementIp,
		AdvertiseEndpoint:   resp.AdvertiseEndpoint,
		NetworkCIDR:         resp.NetworkCidr,
		WGInterface:         resp.WgInterface,
		WGPort:              int(resp.WgPort),
		HelperName:          resp.HelperName,
		CorrosionGossipPort: int(resp.CorrosionGossipPort),
		CorrosionMemberID:   resp.CorrosionMemberId,
		CorrosionAPIToken:   resp.CorrosionApiToken,
		Running:             resp.Running,
	}, nil
}

func (c *Client) ListMachines(ctx context.Context) ([]types.MachineEntry, error) {
	resp, err := c.daemon.ListMachines(ctx, &pb.ListMachinesRequest{})
	if err != nil {
		return nil, grpcErr(err)
	}
	out := make([]types.MachineEntry, len(resp.Machines))
	for i, m := range resp.Machines {
		out[i] = machineFromProto(m)
	}
	return out, nil
}

func (c *Client) UpsertMachine(ctx context.Context, m types.MachineEntry) error {
	_, err := c.daemon.UpsertMachine(ctx, &pb.UpsertMachineRequest{
		Machine: machineToProto(m),
	})
	return grpcErr(err)
}

func (c *Client) RemoveMachine(ctx context.Context, idOrEndpoint string) error {
	_, err := c.daemon.RemoveMachine(ctx, &pb.RemoveMachineRequest{
		IdOrEndpoint: idOrEndpoint,
	})
	return grpcErr(err)
}

func (c *Client) GetPeerHealth(ctx context.Context) ([]types.PeerHealthResponse, error) {
	resp, err := c.daemon.GetPeerHealth(ctx, &pb.GetPeerHealthRequest{})
	if err != nil {
		return nil, grpcErr(err)
	}
	out := make([]types.PeerHealthResponse, len(resp.Messages))
	for i, msg := range resp.Messages {
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
		r.Peers = make([]types.PeerLag, len(msg.Peers))
		for j, p := range msg.Peers {
			pingRTT := time.Duration(p.PingMs * float64(time.Millisecond))
			if p.PingMs < 0 {
				pingRTT = -1
			}
			r.Peers[j] = types.PeerLag{
				NodeID:         p.NodeId,
				Freshness:      time.Duration(p.FreshnessMs) * time.Millisecond,
				Stale:          p.Stale,
				ReplicationLag: time.Duration(p.ReplicationLagMs) * time.Millisecond,
				PingRTT:        pingRTT,
			}
		}
		out[i] = r
	}
	return out, nil
}

// ProxyMachinesContext returns a context that routes gRPC requests through
// the proxy to the specified machines. If nodeIDs is empty, all machines are targeted.
func ProxyMachinesContext(ctx context.Context, nodeIDs []string) context.Context {
	md := metadata.Pairs()
	targets := nodeIDs
	if len(targets) == 0 {
		targets = []string{"*"}
	}
	for _, id := range targets {
		md.Append("machines", id)
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
		Network:                 r.Network,
		NetworkCIDR:             r.NetworkCidr,
		Subnet:                  r.Subnet,
		ManagementIP:            r.ManagementIp,
		WGInterface:             r.WgInterface,
		WGPort:                  int(r.WgPort),
		AdvertiseEndpoint:       r.AdvertiseEndpoint,
		CorrosionName:           r.CorrosionName,
		CorrosionAPIAddr:        r.CorrosionApiAddr,
		CorrosionGossipAddrPort: r.CorrosionGossipAddr,
		DockerNetwork:           r.DockerNetwork,
		SupervisorRunning:       r.SupervisorRunning,
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
	ErrConflict     = errors.New("version conflict")
	ErrNotFound     = errors.New("not found")
	ErrValidation   = errors.New("validation error")
	ErrUnavailable  = errors.New("unavailable")
	ErrPrecondition = errors.New("precondition failed")

	ErrNetworkNotConfigured       = errors.New("network is not configured")
	ErrRuntimeNotReadyForServices = errors.New("runtime is not ready for service operations")
	ErrNoMachinesAvailable        = errors.New("no schedulable machines available")
	ErrNetworkDestroyHasWorkloads = errors.New("network destroy blocked by managed workloads")
	ErrNetworkDestroyHasMachines  = errors.New("network destroy blocked by attached machines")
)

const (
	errorInfoMetadataPreconditionCode = "precondition_code"
	errorInfoMetadataRemediationHint  = "remediation_hint"
)

type hintedPreconditionError struct {
	err  error
	hint string
}

func (e *hintedPreconditionError) Error() string {
	if e == nil || e.err == nil {
		return ""
	}
	return e.err.Error()
}

func (e *hintedPreconditionError) Unwrap() error {
	if e == nil {
		return nil
	}
	return e.err
}

// PreconditionHint returns a structured remediation hint attached to a mapped
// precondition error. It returns an empty string when no hint is available.
func PreconditionHint(err error) string {
	var hinted *hintedPreconditionError
	if !errors.As(err, &hinted) || hinted == nil {
		return ""
	}
	return strings.TrimSpace(hinted.hint)
}

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
	case codes.Aborted:
		return fmt.Errorf("%w: %s", ErrConflict, st.Message())
	case codes.FailedPrecondition:
		if mapped := mapPreconditionDetail(st); mapped != nil {
			return mapped
		}
		base := fmt.Errorf("%w: %s", ErrPrecondition, st.Message())
		if hint := preconditionHintFromStatus(st, ""); hint != "" {
			return &hintedPreconditionError{err: base, hint: hint}
		}
		return base
	default:
		return errors.New(st.Message())
	}
}

func mapPreconditionDetail(st *status.Status) error {
	if st == nil {
		return nil
	}
	for _, detail := range st.Details() {
		failure, ok := detail.(*errdetails.PreconditionFailure)
		if !ok || failure == nil {
			continue
		}
		for _, violation := range failure.Violations {
			if violation == nil {
				continue
			}
			hint := preconditionHintFromStatus(st, violation.Type)
			switch violation.Type {
			case string(types.PreconditionNetworkNotConfigured):
				return wrapPreconditionWithHint(
					fmt.Errorf("%w: %w: %s", ErrPrecondition, ErrNetworkNotConfigured, st.Message()),
					hint,
				)
			case string(types.PreconditionRuntimeNotReadyForServices):
				return wrapPreconditionWithHint(
					fmt.Errorf("%w: %w: %s", ErrPrecondition, ErrRuntimeNotReadyForServices, st.Message()),
					hint,
				)
			case string(types.PreconditionDeployNoMachinesAvailable):
				return wrapPreconditionWithHint(
					fmt.Errorf("%w: %w: %s", ErrPrecondition, ErrNoMachinesAvailable, st.Message()),
					hint,
				)
			case string(types.PreconditionNetworkDestroyHasWorkloads):
				return wrapPreconditionWithHint(
					fmt.Errorf("%w: %w: %s", ErrPrecondition, ErrNetworkDestroyHasWorkloads, st.Message()),
					hint,
				)
			case string(types.PreconditionNetworkDestroyHasMachines):
				return wrapPreconditionWithHint(
					fmt.Errorf("%w: %w: %s", ErrPrecondition, ErrNetworkDestroyHasMachines, st.Message()),
					hint,
				)
			}
		}
	}
	return nil
}

func preconditionHintFromStatus(st *status.Status, code string) string {
	if st == nil {
		return ""
	}
	for _, detail := range st.Details() {
		errInfo, ok := detail.(*errdetails.ErrorInfo)
		if !ok || errInfo == nil {
			continue
		}
		hint := strings.TrimSpace(errInfo.Metadata[errorInfoMetadataRemediationHint])
		if hint == "" {
			continue
		}
		metadataCode := strings.TrimSpace(errInfo.Metadata[errorInfoMetadataPreconditionCode])
		if code == "" || metadataCode == "" || metadataCode == code {
			return hint
		}
	}
	return ""
}

func wrapPreconditionWithHint(err error, hint string) error {
	hint = strings.TrimSpace(hint)
	if hint == "" {
		return err
	}
	return &hintedPreconditionError{err: err, hint: hint}
}
